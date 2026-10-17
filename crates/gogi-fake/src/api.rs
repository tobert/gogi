//! The council v2 routes over the toy model.

use std::collections::{HashSet, VecDeque};

use gogi_proto::canon::{sha256_hex, spec_id_of};
use gogi_proto::context::Context;
use gogi_proto::decision::{DecisionRequest, PoolMethod, PoolWeights, check_queue_schema, queue_call};
use gogi_proto::generation::GenerationRequest;
use gogi_proto::math::{self, Row, WeightSpec};
use gogi_proto::message::{Message, ToolCall};
use gogi_proto::pins::{PinsPut, is_owner};
use gogi_proto::reads::ReadsRequest;
use gogi_proto::spec::{Question, QuestionKind, SpecRules, parse_inline, parse_spec};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::model::{self, State, VOCAB};
use crate::state::{BYTES_PER_TOKEN, IDENTITY_KEY, Pins, Store, snapshot_id};

pub const MODEL: &str = "gogi-toy-1";
pub const TEMPLATE: &str = "gogi-toy-chat-1";

pub enum Resp {
    Json(u16, Value),
    Empty(u16),
    /// A server-sent event stream: the events, then the end of the connection.
    Sse(Vec<(&'static str, Value)>),
}

fn err(status: u16, kind: &str, message: impl Into<String>, param: Option<&str>) -> Resp {
    let mut e = json!({"type": kind, "message": message.into()});
    if let Some(p) = param {
        e["param"] = json!(p);
    }
    Resp::Json(status, json!({"error": e}))
}

fn bad(message: impl Into<String>) -> Resp {
    err(400, "invalid_request", message, None)
}

fn bad_param(param: &str, message: impl Into<String>) -> Resp {
    err(400, "invalid_request", message, Some(param))
}

fn not_found(message: impl Into<String>) -> Resp {
    err(404, "not_found", message, None)
}

fn invalid(e: gogi_proto::Invalid) -> Resp {
    match e.param {
        Some(p) => bad_param(&p, e.message),
        None => bad(e.message),
    }
}

fn context_full(prompt: usize, limit: usize) -> Resp {
    Resp::Json(
        413,
        json!({"error": {"type": "context_full", "message": format!("the prompt takes {} of the {} tokens a context holds", prompt, limit),
                         "prompt_tokens": prompt, "context_tokens": limit}}),
    )
}

fn parse<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T, Resp> {
    serde_json::from_slice(body).map_err(|e| bad(format!("{} body: {}", what, e)))
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn identity(store: &Store) -> Value {
    json!({
        "model": MODEL, "aliases": [], "weight_hash": "0x0", "tokenizer_hash": "bytes-v1",
        "engine": format!("gogi-fake:{}", env!("CARGO_PKG_VERSION")), "device": "cpu",
        "template": TEMPLATE,
        "limits": {"context_tokens": store.cfg.context_tokens, "state_bytes": 65536, "contexts_per_decision": 8,
                   "questions_per_spec": 16, "choice_options": 26, "default_timeout_ms": 30000,
                   "pin_bytes": store.cfg.pin_bytes, "snapshot_bytes": BYTES_PER_TOKEN,
                   "decision_context_tokens": store.cfg.decision_context_tokens},
        "capabilities": ["reads", "generate", "pins", "dry_run", "leave_one_out"],
        "extensions": [],
    })
}

pub fn handle(store: &mut Store, method: &str, path: &str, body: &[u8]) -> Resp {
    store.expire_pins();
    let Some(rest) = path.strip_prefix("/council/v2/") else { return not_found("unknown route") };
    let parts: Vec<&str> = rest.split('/').collect();
    match (method, parts.as_slice()) {
        ("GET", ["identity"]) => Resp::Json(200, identity(store)),
        ("POST", ["contexts"]) => build_route(store, body),
        ("GET", ["snapshots", id]) => snapshot_route(store, &unescape(id)),
        ("PUT", ["pins", owner]) => pins_put(store, &unescape(owner), body),
        ("GET", ["pins", owner]) => match store.pins.get(&unescape(owner)) {
            Some(p) => Resp::Json(200, json!({"owner": unescape(owner), "snapshots": p.snapshots, "expires_in_s": store.expires_in(p.expires)})),
            None => not_found(format!("{} pins nothing", unescape(owner))),
        },
        ("DELETE", ["pins", owner]) => {
            if store.pins.remove(&unescape(owner)).is_none() {
                return not_found(format!("{} pins nothing", unescape(owner)));
            }
            store.save();
            Resp::Empty(204)
        }
        ("POST", ["specs"]) => specs_post(store, body),
        ("GET", ["specs"]) => Resp::Json(200, json!({"specs": store.specs.iter().map(|(id, s)| held_spec(id, s)).collect::<Vec<_>>()})),
        ("GET", ["specs", id]) => match store.specs.get(*id) {
            Some(s) => Resp::Json(200, held_spec(id, s)),
            None => not_found(format!("no held spec {}", id)),
        },
        ("DELETE", ["specs", id]) => {
            if store.specs.shift_remove(*id).is_none() {
                return not_found(format!("no held spec {}", id));
            }
            store.save();
            Resp::Empty(204)
        }
        ("POST", ["decisions"]) => decisions(store, body),
        ("POST", ["reads"]) => reads(store, body),
        ("POST", ["generations"]) => generations(store, body),
        _ => not_found("unknown route"),
    }
}

// ---- specs

fn held_spec(id: &str, spec: &Value) -> Value {
    json!({"spec_id": id, "spec": spec, "template": TEMPLATE})
}

fn rules() -> SpecRules {
    SpecRules::default()
}

fn specs_post(store: &mut Store, body: &[u8]) -> Resp {
    let v: Value = match parse(body, "spec") {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) = parse_spec(&v, &rules()) {
        return invalid(e);
    }
    let id = match spec_id_of(&v) {
        Ok(id) => id,
        Err(e) => return bad(e),
    };
    store.specs.insert(id.clone(), v.clone());
    store.save();
    Resp::Json(200, held_spec(&id, &v))
}

// ---- snapshots and builds

struct Built {
    /// The context's tokens, without a generation prompt.
    tokens: Vec<u32>,
    cuts: Vec<(String, model::Cut)>,
    head: String,
    kept: usize,
    from: Option<String>,
}

impl Built {
    fn infos(&self, store: &Store) -> Vec<Value> {
        self.cuts
            .iter()
            .map(|(id, c)| json!({"id": id, "message": c.message, "tokens": c.tokens, "tier": "host", "bytes": store.own_tokens(id) as u64 * BYTES_PER_TOKEN}))
            .collect()
    }
}

/// Renders a context and builds it: starts from the longest boundary held, and holds each
/// boundary and the end unless `dry_run`.
fn build(store: &mut Store, c: &Context, dry_run: bool) -> Built {
    let r = model::render(&c.tools, c.thinks(), &c.messages, false);
    let mut counts = Vec::new();
    let mut cuts = Vec::new();
    for cut in &r.cuts {
        counts.push(cut.tokens);
        cuts.push((snapshot_id(&r.tokens[..cut.tokens], &counts), *cut));
    }
    let head = cuts.last().unwrap().0.clone();
    let best = cuts.iter().rev().find(|(id, _)| store.has(id));
    let (kept, from) = best.map_or((0, None), |(id, cut)| (cut.tokens, Some(id.clone())));
    if !dry_run {
        let mut parent = None;
        for (id, cut) in &cuts {
            store.hold(id.clone(), &r.tokens[..cut.tokens], parent);
            parent = Some(id.clone());
        }
    }
    Built { tokens: r.tokens, cuts, head, kept, from }
}

fn build_route(store: &mut Store, body: &[u8]) -> Resp {
    let req: gogi_proto::ContextBuild = match parse(body, "context") {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = req.context.check() {
        return invalid(e);
    }
    let dry_run = req.dry_run.unwrap_or(false);
    let limit = store.cfg.context_tokens;
    let n = model::render(&req.context.tools, req.context.thinks(), &req.context.messages, false).end;
    if n >= limit {
        return context_full(n, limit);
    }
    let b = build(store, &req.context, dry_run);
    let snapshots = if dry_run { vec![] } else { b.infos(store) };
    Resp::Json(200, json!({"head": b.head, "tokens": n, "kept": b.kept, "fed": n - b.kept, "from": b.from, "dry_run": dry_run, "snapshots": snapshots}))
}

fn snapshot_route(store: &Store, id: &str) -> Resp {
    let Some(s) = store.snaps.get(id) else { return not_found(format!("no snapshot {}", id)) };
    let pins: Vec<Value> = store
        .pins
        .iter()
        .filter(|(_, p)| p.snapshots.iter().any(|x| x == id))
        .map(|(o, p)| json!({"owner": o, "expires_in_s": store.expires_in(p.expires)}))
        .collect();
    Resp::Json(200, json!({"id": id, "tokens": s.tokens.len(), "tier": "host", "bytes": store.own_tokens(id) as u64 * BYTES_PER_TOKEN, "pins": pins}))
}

fn pins_put(store: &mut Store, owner: &str, body: &[u8]) -> Resp {
    if !is_owner(owner) {
        return bad_param("owner", "an owner is printable text of up to 128 characters, without a slash");
    }
    let req: PinsPut = match parse(body, "pins") {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let held: Vec<bool> = req.snapshots.iter().map(|id| store.has(id)).collect();
    let ids: Vec<String> = req.snapshots.iter().zip(&held).filter(|(_, h)| **h).map(|(i, _)| i.clone()).collect();
    if store.bytes_of_closure(&ids) > store.cfg.pin_bytes {
        return err(413, "too_large", "these pins hold more than pin_bytes", Some("pin_bytes"));
    }
    let snapshots: Vec<Value> = req.snapshots.iter().zip(&held).map(|(id, h)| json!({"id": id, "held": h})).collect();
    let expires_in = if ids.is_empty() {
        store.pins.remove(owner);
        Value::Null
    } else {
        let expires = store.now() + req.ttl_s;
        store.pins.insert(owner.to_string(), Pins { snapshots: ids, expires });
        json!(req.ttl_s)
    };
    store.save();
    Resp::Json(200, json!({"owner": owner, "snapshots": snapshots, "expires_in_s": expires_in}))
}

// ---- reads

fn letter_tokens(letters: &[String]) -> Result<Vec<u32>, Resp> {
    let mut ids = Vec::new();
    for l in letters {
        let t = model::tokenize_suffix(l);
        if t.len() != 1 || ids.contains(&t[0]) {
            return Err(bad_param("letters", format!("letter {:?} is not one token of its own", l)));
        }
        ids.push(t[0]);
    }
    Ok(ids)
}

fn reads(store: &mut Store, body: &[u8]) -> Resp {
    let req: ReadsRequest = match parse(body, "reads") {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let letters = match letter_tokens(&req.letters) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let suffixes: Vec<Vec<u32>> = req.suffixes.iter().map(|s| model::tokenize_suffix(s)).collect();
    let longest = suffixes.iter().map(Vec::len).max().unwrap_or(0);
    let top = req.top.unwrap_or(8).min(64);
    let control: Vec<Value> = req
        .suffixes
        .iter()
        .enumerate()
        .flat_map(|(i, s)| model::control_hits(s).into_iter().map(move |t| json!({"where": format!("suffix:{}", i), "token": t})))
        .collect();
    let mut out = Vec::new();
    for (ci, c) in req.contexts.iter().enumerate() {
        let n = model::render(&c.tools, c.thinks(), &c.messages, false).end;
        if n + longest >= store.cfg.context_tokens {
            return context_full(n + longest, store.cfg.context_tokens);
        }
        let b = build(store, c, false);
        let base = State::after(&b.tokens);
        for (si, suffix) in suffixes.iter().enumerate() {
            let st = suffix.iter().fold(base, |s, &t| s.step(t));
            let lp = model::log_softmax(&st.logits());
            let mut logprobs = Map::new();
            let mut ls = Vec::new();
            for (l, &id) in req.letters.iter().zip(&letters) {
                logprobs.insert(l.clone(), json!(lp[id as usize]));
                ls.push(lp[id as usize]);
            }
            let mut order: Vec<usize> = (0..VOCAB).collect();
            order.sort_by(|a, b| lp[*b].total_cmp(&lp[*a]).then(a.cmp(b)));
            let tops: Vec<Value> = order.iter().take(top).map(|&i| json!({"token": model::piece(i as u32), "id": i, "logprob": lp[i]})).collect();
            out.push(json!({"context": ci, "snapshot": b.head, "suffix": si, "logprobs": logprobs, "mass": math::log_sum_exp(&ls), "top": tops}));
        }
    }
    Resp::Json(200, json!({"reads": out, "signals": {"control_text": control}}))
}

// ---- generations

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Think,
    Answer,
}

/// Picks a token among `cands` by their logits: the largest at temperature 0, else a draw.
/// Returns the token, its probability among the candidates, and their entropy.
fn pick(cands: &[u32], logits: &[f64; VOCAB], temperature: f64, top_k: usize, top_p: f64, min_p: f64, rng: &mut Rng) -> (u32, f64, f64) {
    let max = cands.iter().map(|&c| logits[c as usize]).fold(f64::NEG_INFINITY, f64::max);
    let w: Vec<f64> = cands.iter().map(|&c| (logits[c as usize] - max).exp()).collect();
    let z: f64 = w.iter().sum();
    let p: Vec<f64> = w.iter().map(|x| x / z).collect();
    let h: f64 = -p.iter().map(|x| x * x.ln()).sum::<f64>();
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|a, b| p[*b].total_cmp(&p[*a]).then(cands[*a].cmp(&cands[*b])));
    if temperature == 0.0 {
        return (cands[order[0]], p[order[0]], h);
    }
    let mut kept: Vec<usize> = Vec::new();
    let mut acc = 0.0;
    for &i in &order {
        if (top_k > 0 && kept.len() >= top_k) || (!kept.is_empty() && (acc >= top_p || p[i] < min_p * p[order[0]])) {
            break;
        }
        kept.push(i);
        acc += p[i];
    }
    let t: Vec<f64> = kept.iter().map(|&i| (logits[cands[i] as usize] - max) / temperature).collect();
    let tm = t.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let tw: Vec<f64> = t.iter().map(|x| (x - tm).exp()).collect();
    let mut r = rng.unit() * tw.iter().sum::<f64>();
    for (k, &i) in kept.iter().enumerate() {
        r -= tw[k];
        if r <= 0.0 {
            return (cands[i], p[i], h);
        }
    }
    let i = *kept.last().unwrap();
    (cands[i], p[i], h)
}

fn generations(store: &mut Store, body: &[u8]) -> Resp {
    let req: GenerationRequest = match parse(body, "generation") {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let limit = store.cfg.context_tokens;
    let max_tokens = req.max_tokens.map_or(256.min(limit), |n| n as usize);
    if max_tokens > limit {
        return bad_param("max_tokens", format!("max_tokens is 1 to {}", limit));
    }
    let c = &req.context;
    let thinking = c.thinks();
    let prompt = model::render(&c.tools, thinking, &c.messages, true);
    if prompt.tokens.len() + 1 >= limit {
        return context_full(prompt.tokens.len(), limit);
    }
    let b = build(store, c, false);
    let sample = req.sample.clone().unwrap_or_default();
    let temperature = sample.temperature.unwrap_or(1.0);
    let seed = sample.seed.unwrap_or_else(|| u64::from_le_bytes(sha256_hex(&prompt.tokens.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>()).as_bytes()[..8].try_into().unwrap()));
    let mut rng = Rng(seed);
    let trigger = !c.tools.is_empty() && c.messages.last().and_then(|m| m.content.as_deref()).is_some_and(|t| t.contains("CALL"));

    let mut toks = prompt.tokens.clone();
    let mut st = State::after(&toks);
    let mut phase = if thinking { Phase::Think } else { Phase::Answer };
    let (mut reasoning, mut content): (Vec<u8>, Vec<u8>) = (vec![], vec![]);
    let mut forced: VecDeque<u32> = VecDeque::new();
    let mut queued = false;
    let mut in_call = false;
    let mut calls: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut events = Vec::new();
    let mut finish = "length";
    let mut n = 0;
    let letters: Vec<u32> = (b'a'..=b'z').map(u32::from).chain([u32::from(b' ')]).collect();
    while n < max_tokens && toks.len() + 1 < limit {
        if phase == Phase::Answer && trigger && !queued {
            queued = true;
            let name = c.tool_names().first().copied().unwrap_or("tool").to_string();
            forced.push_back(model::CALL);
            forced.extend(format!("{}({{}})", name).bytes().map(u32::from));
            forced.extend([model::ENDCALL, model::END]);
            calls.push((name, Map::new()));
        }
        let (tok, p, h) = if let Some(f) = forced.pop_front() {
            (f, 1.0, 0.0)
        } else {
            let mut cands = letters.clone();
            cands.push(if phase == Phase::Think { model::ENDTHINK } else { model::END });
            pick(&cands, &st.logits(), temperature, sample.top_k.unwrap_or(0) as usize, sample.top_p.unwrap_or(1.0), sample.min_p.unwrap_or(0.0), &mut rng)
        };
        n += 1;
        toks.push(tok);
        st = st.step(tok);
        let kind = match (phase, tok) {
            (_, model::CALL) => {
                in_call = true;
                "tool"
            }
            (_, model::ENDCALL) => {
                in_call = false;
                "tool"
            }
            _ if in_call => "tool",
            (Phase::Think, _) => "think",
            _ => "answer",
        };
        events.push(("token", json!({"id": tok, "piece": model::piece(tok), "phase": kind, "p": p, "h": h, "n": n, "tps": 0.0})));
        match (phase, tok) {
            (Phase::Think, model::ENDTHINK) => phase = Phase::Answer,
            (_, model::END) => {
                finish = "stop";
                break;
            }
            (Phase::Think, t) => reasoning.push(t as u8),
            (Phase::Answer, t) if !in_call && t < 256 => content.push(t as u8),
            _ => {}
        }
    }
    let (reasoning, content) = (String::from_utf8_lossy(&reasoning).into_owned(), String::from_utf8_lossy(&content).into_owned());
    let tool_calls: Vec<ToolCall> = calls.into_iter().enumerate().map(|(k, (name, arguments))| ToolCall { id: format!("call_{}_{}", prompt.tokens.len(), k), name, arguments }).collect();
    let reply = Message {
        reasoning: Some(reasoning.clone()).filter(|r| !r.is_empty()),
        tool_calls: tool_calls.clone(),
        ..Message::assistant(content.clone())
    };
    let reply = Message { content: Some(content.clone()).filter(|c| !c.is_empty()), ..reply };
    let mut messages = c.messages.clone();
    messages.push(reply);
    let after = Context { messages, ..c.clone() };
    let held = build(store, &after, false);
    let done = json!({
        "content": content, "reasoning": reasoning,
        "tool_calls": tool_calls.iter().map(|t| json!({"id": t.id, "name": t.name, "arguments": t.arguments})).collect::<Vec<_>>(),
        "finish": finish,
        "usage": {"prompt": prompt.tokens.len(), "kept": b.kept, "fed": prompt.tokens.len() - b.kept, "completion": n},
        "head": held.head, "snapshots": held.infos(store), "seed": seed, "ms": {"prefill": 0.0, "decode": 0.0}, "model": MODEL,
    });
    if req.stream.unwrap_or(false) {
        events.push(("done", done));
        Resp::Sse(events)
    } else {
        Resp::Json(200, done)
    }
}

// ---- decisions

struct Asked<'a> {
    questions: &'a [Question],
    asked: Vec<usize>,
    /// Question index to the chosen option positions.
    chosen: Vec<Vec<usize>>,
}

fn decisions(store: &mut Store, body: &[u8]) -> Resp {
    let t0 = std::time::Instant::now();
    let req: DecisionRequest = match parse(body, "decision") {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Some(t) = &req.think {
        if t.tokens > 0 {
            return bad_param("think", "think above 0 tokens needs the think capability, which this server does not have");
        }
    }
    if let Some(m) = &req.model {
        if m != MODEL {
            return bad_param("model", format!("model {} is not this server's", m));
        }
    }
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let (spec_id, questions, instructions, label) = match (&req.spec_id, &req.questions) {
        (Some(id), None) => {
            let Some(v) = store.specs.get(id) else { return not_found(format!("no held spec {}; POST it again", id)) };
            let s = match parse_spec(v, &rules()) {
                Ok(s) => s,
                Err(e) => return invalid(e),
            };
            (Some(id.clone()), s.questions, s.instructions, s.input_label)
        }
        (None, Some(q)) => match parse_inline(q, &rules()) {
            Ok(qs) => (None, qs, String::new(), "Input".to_string()),
            Err(e) => return invalid(e),
        },
        _ => return bad("exactly one of spec_id or questions"),
    };
    let asked: Vec<usize> = match &req.ask {
        None => (0..questions.len()).collect(),
        Some(ids) => {
            if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
                return bad_param("ask", "ask repeats a question");
            }
            let mut out = Vec::new();
            for id in ids {
                match questions.iter().position(|q| &q.id == id) {
                    Some(i) => out.push(i),
                    None => return bad_param("ask", format!("the spec has no question {}", id)),
                }
            }
            out.sort();
            out
        }
    };
    let mut chosen: Vec<Vec<usize>> = questions.iter().map(|q| (0..q.answer_keys().len()).collect()).collect();
    for (qid, list) in req.options.iter().flatten() {
        let Some(qi) = questions.iter().position(|q| &q.id == qid) else { return bad_param("options", format!("the spec has no question {}", qid)) };
        let QuestionKind::Choice { options } = &questions[qi].kind else { return bad_param("options", format!("{} is not a choice question", qid)) };
        let names: Option<Vec<&str>> = list.as_array().and_then(|a| a.iter().map(|v| v.as_str()).collect());
        let Some(names) = names else { return bad_param("options", "options name a list of option names") };
        if names.len() < 2 || names.iter().collect::<HashSet<_>>().len() != names.len() {
            return bad_param("options", "narrow to at least 2 distinct options");
        }
        let mut idx = Vec::new();
        for n in names {
            match options.iter().position(|(o, _)| o == n) {
                Some(i) => idx.push(i),
                None => return bad_param("options", format!("{} has no option {}", qid, n)),
            }
        }
        idx.sort();
        chosen[qi] = idx;
    }
    let state_text = match &req.state {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if state_text.len() > 65536 {
        return err(413, "too_large", "state is past the limit", Some("state_bytes"));
    }
    let pool = req.pool.clone().unwrap_or_default();
    let method = pool.method.unwrap_or(PoolMethod::Linear);
    let weights_kind = pool.weights.unwrap_or(PoolWeights::Uniform);
    let ask = Asked { questions: &questions, asked, chosen };
    let mut control: Vec<Value> = Vec::new();
    for (where_, text) in [("state", state_text.as_str()), ("spec", instructions.as_str())] {
        control.extend(model::control_hits(text).into_iter().map(|t| json!({"where": where_, "token": t})));
    }

    // Each read: the context's head, then the answers' log probabilities by asked question.
    let mut reads: Vec<(Option<String>, Vec<Vec<f64>>, usize, String)> = Vec::new();
    let mut fed = 0usize;
    let mut input_tokens = 0usize;
    let contexts: Vec<Option<&Context>> = if req.contexts.is_empty() { vec![None] } else { req.contexts.iter().map(Some).collect() };
    for c in contexts {
        let (head, base_tokens, queue) = match c {
            None => (None, vec![model::BOS], None),
            Some(c) => {
                let n = model::render(&c.tools, c.thinks(), &c.messages, false).end;
                if n > store.cfg.decision_context_tokens {
                    return err(413, "too_large", format!("context is {} tokens; decisions read contexts of up to {}", n, store.cfg.decision_context_tokens), Some("decision_context_tokens"));
                }
                let b = build(store, c, false);
                fed += n - b.kept;
                (Some(b.head), b.tokens, queue_call(c).map(|call| (c, call.id.clone())))
            }
        };
        let sorted: Vec<usize> = {
            let mut o = ask.asked.clone();
            o.sort_by(|&x, &y| ask.questions[x].id.cmp(&ask.questions[y].id));
            o
        };
        let mut lps: Vec<Option<Vec<f64>>> = vec![None; ask.questions.len()];
        let rendered;
        if let Some((c, call_id)) = queue {
            let asked_ids: Vec<(&str, usize)> = ask.asked.iter().map(|&qi| (ask.questions[qi].id.as_str(), ask.questions[qi].answer_keys().len())).collect();
            if let Err(e) = check_queue_schema(c, &asked_ids) {
                return invalid(e);
            }
            let mut messages = c.messages.clone();
            messages.push(Message::tool(call_id, state_text.clone()));
            let case = model::render(&c.tools, c.thinks(), &messages, true).tokens;
            input_tokens = case.len().saturating_sub(base_tokens.len());
            let mut toks = case;
            model::push_text(&mut toks, &format!("call:{}{{answers:{{", c.queue.as_deref().unwrap_or("")));
            for &qi in &sorted {
                let q = &ask.questions[qi];
                model::push_text(&mut toks, &format!("{}:\"", q.id));
                let lp = model::log_softmax(&State::after(&toks).logits());
                let read: Vec<f64> = ask.chosen[qi].iter().map(|&j| lp[usize::from(b'A') + j]).collect();
                let best = (0..read.len()).fold(0, |b, i| if read[i] > read[b] { i } else { b });
                model::push_text(&mut toks, &format!("{}\",", (b'A' + ask.chosen[qi][best] as u8) as char));
                lps[qi] = Some(read);
            }
            rendered = sha256_hex(toks.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>().as_slice());
            fed += toks.len() - base_tokens.len();
        } else {
            let mut case = Vec::new();
            model::push_text(&mut case, &format!("{}\n{}: {}", instructions, label, state_text));
            input_tokens = case.len();
            let mut text_all = base_tokens.clone();
            text_all.push(model::USER);
            text_all.extend(&case);
            text_all.extend([model::END, model::ASST]);
            for &qi in &ask.asked {
                let q = &ask.questions[qi];
                let mut toks = text_all.clone();
                model::push_text(&mut toks, &format!("{} {}:", q.id, q.instructions));
                let lp = model::log_softmax(&State::after(&toks).logits());
                lps[qi] = Some(ask.chosen[qi].iter().map(|&j| lp[usize::from(b'A') + j]).collect());
                fed += toks.len() - base_tokens.len();
            }
            rendered = sha256_hex(text_all.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>().as_slice());
        }
        reads.push((head, ask.asked.iter().map(|&qi| lps[qi].take().unwrap()).collect(), reads.len(), rendered));
    }

    let named: Vec<Option<String>> = (0..reads.len()).map(|i| (!req.contexts.is_empty()).then(|| i.to_string())).collect();
    let mut read_answers: Vec<Map<String, Value>> = vec![Map::new(); reads.len()];
    let mut answers = Map::new();
    let mut normalized = Map::new();
    for (ai, &qi) in ask.asked.iter().enumerate() {
        let q = &ask.questions[qi];
        let keys: Vec<String> = ask.chosen[qi].iter().map(|&j| q.answer_keys()[j].clone()).collect();
        let rows: Result<Vec<Row>, _> = reads.iter().map(|r| Row::from_logprobs(&r.1[ai])).collect();
        let rows = match rows {
            Ok(r) => r,
            Err(e) => return bad(e.to_string()),
        };
        let spec = match weights_kind {
            PoolWeights::Uniform => WeightSpec::Uniform,
            PoolWeights::Mass => WeightSpec::Mass,
            PoolWeights::Given => WeightSpec::Given(pool.values.clone().unwrap_or_default()),
        };
        let p = match math::pool(&rows, method, &spec) {
            Ok(p) => p,
            Err(e) => return bad(e.to_string()),
        };
        normalized.insert(q.id.clone(), json!(p.weights));
        let map_of = |v: &[f64]| Value::Object(keys.iter().zip(v).map(|(k, x)| (k.clone(), json!(x))).collect::<Map<_, _>>());
        let legend = |levels: &[String]| Value::Object(levels.iter().enumerate().map(|(i, l)| (i.to_string(), json!(l))).collect::<Map<_, _>>());
        for (ri, row) in rows.iter().enumerate() {
            let lp = &reads[ri].1[ai];
            let top = math::argmax(&row.probs);
            let v = match &q.kind {
                QuestionKind::Choice { .. } => json!({"type": "choice", "choice": keys[top], "probabilities": map_of(&row.probs), "confidence": math::confidence(row.mass, &row.probs), "logprobs": map_of(lp), "mass": row.mass}),
                QuestionKind::Score { levels } => json!({"type": "score", "score": math::score(&row.probs), "legend": legend(levels), "probabilities": map_of(&row.probs), "confidence": math::confidence(row.mass, &row.probs), "logprobs": map_of(lp), "mass": row.mass}),
                _ => json!({"type": "noul", "noul": row.probs[0], "logprobs": map_of(lp), "mass": row.mass}),
            };
            read_answers[ri].insert(q.id.clone(), v);
        }
        let loo = p.leave_one_out.as_ref().map(|l| {
            Value::Object(l.iter().enumerate().map(|(i, x)| (named[i].clone().unwrap_or_default(), x.as_ref().map_or(Value::Null, |v| if matches!(q.kind, QuestionKind::Noul { .. }) { json!(v[0]) } else { map_of(v) }))).collect::<Map<_, _>>())
        });
        let mut v = match &q.kind {
            QuestionKind::Choice { .. } => json!({"type": "choice", "choice": keys[math::argmax(&p.probs)], "probabilities": map_of(&p.probs), "confidence": p.confidence(), "agree": p.agree, "spread": p.spread}),
            QuestionKind::Score { levels } => json!({"type": "score", "score": math::score(&p.probs), "legend": legend(levels), "probabilities": map_of(&p.probs), "confidence": p.confidence(), "agree": p.agree, "spread": p.spread}),
            _ => json!({"type": "noul", "noul": p.probs[0], "agree": p.agree, "spread": p.spread}),
        };
        if let Some(l) = loo {
            v["leave_one_out"] = l;
        }
        answers.insert(q.id.clone(), v);
    }
    let reads_json: Vec<Value> = reads
        .iter()
        .enumerate()
        .map(|(i, (head, _, _, rendered))| json!({"context": if named[i].is_some() { json!(i) } else { Value::Null }, "snapshot": head, "answers": read_answers[i], "rendered_sha256": rendered, "tokens": 0, "ms": 0.0}))
        .collect();
    let mut identity = json!({"model": MODEL, "weight_hash": "0x0", "tokenizer_hash": "bytes-v1", "template": TEMPLATE, "engine": format!("gogi-fake:{}", env!("CARGO_PKG_VERSION"))});
    if let Some(s) = spec_id {
        identity["spec_id"] = json!(s);
    }
    let _ = IDENTITY_KEY;
    Resp::Json(
        200,
        json!({"model": MODEL, "answers": answers, "reads": reads_json,
               "pool": {"method": method, "weights": weights_kind, "normalized": normalized},
               "signals": {"control_text": control}, "identity": identity,
               "usage": {"input_tokens": input_tokens, "output_tokens": 0, "fed_tokens": fed},
               "queue_ms": 0.0, "ms": t0.elapsed().as_secs_f64() * 1000.0}),
    )
}
