//! The council API routes over the toy model.

use std::collections::{HashSet, VecDeque};

use gogi_proto::canon::{sha256_hex, spec_id_of};
use gogi_proto::context::{Context, Usage};
use gogi_proto::decision::{DecisionRequest, PoolMethod, PoolWeights, check_queue_schema, queue_call};
use gogi_proto::error::{ErrorBody, ErrorType};
use gogi_proto::generation::{GenerationRequest, Phase as EventPhase, StreamEvent, TokenEvent};
use gogi_proto::identity::{ModelStatus, ServerIdentity};
use gogi_proto::math::{self, Row, WeightSpec};
use gogi_proto::message::{Message, ToolCall};
use gogi_proto::pins::{PinsPut, is_owner};
use gogi_proto::reads::ReadsRequest;
use gogi_proto::spec::{Question, QuestionKind, SpecRules, parse_spec};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::model::{self, State, VOCAB};
use crate::state::{BYTES_PER_TOKEN, FakeModel, Pins, Store, snapshot_id};

pub const TEMPLATE: &str = "gogi-toy-chat-2";

/// The longest case, in bytes.
const CASE_BYTES: usize = 65536;

pub enum Resp {
    Json(u16, Value),
    Empty(u16),
    /// A server-sent event stream: the events, then the end of the connection.
    Sse(Vec<StreamEvent>),
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
        json!({"error": {"type": "context_full", "message": format!("the prompt is {} tokens; context_tokens is {}", prompt, limit),
                         "prompt_tokens": prompt, "context_tokens": limit}}),
    )
}

/// Decodes a request body. A missing or unknown member is a `400` that names it in `param`.
fn parse<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T, Resp> {
    serde_json::from_slice(body).map_err(|e| {
        let text = e.to_string();
        let field = ["missing field `", "unknown field `"].iter().find_map(|p| text.split_once(p).and_then(|(_, rest)| rest.split_once('`')).map(|(f, _)| f.to_string()));
        let message = format!("nothing ran: the {} body does not decode: {}", what, text);
        match field {
            Some(f) => bad_param(&f, message),
            None => bad(message),
        }
    })
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

/// The identity fields a served model's numbers depend on.
fn identity_of(m: &FakeModel) -> Value {
    json!({"model": m.name, "weight_hash": format!("0x{:016X}", m.seed), "tokenizer_hash": "bytes-v1", "template": TEMPLATE,
           "engine": format!("gogi-fake:{}", env!("CARGO_PKG_VERSION"))})
}

pub fn identity(store: &Store) -> Value {
    let models: Vec<Value> = store
        .cfg
        .models
        .iter()
        .map(|m| {
            let mut v = identity_of(m);
            v["aliases"] = json!(m.aliases);
            v["status"] = json!(m.status);
            v["device"] = json!("cpu");
            v["limits"] = json!({"context_tokens": store.cfg.context_tokens, "decision_context_tokens": store.cfg.decision_context_tokens,
                                 "snapshot_bytes": BYTES_PER_TOKEN, "contexts_per_decision": 8, "default_timeout_ms": 30000});
            v["capabilities"] = json!(["reads", "generate", "dry_run", "leave_one_out"]);
            v
        })
        .collect();
    json!({
        "models": models,
        "limits": {"case_bytes": CASE_BYTES, "choice_options": 26, "questions_per_spec": 16, "pin_bytes": store.cfg.pin_bytes},
        "capabilities": ["pins"],
        "extensions": [],
    })
}

/// The served model a request names, or the `400` or `503` that refuses it.
fn route(store: &Store, model: &str) -> Result<FakeModel, Resp> {
    let id: ServerIdentity = serde_json::from_value(identity(store)).expect("the fake's identity decodes");
    let m = id.route(model).map_err(invalid)?;
    if m.status != ModelStatus::Ready {
        return Err(Resp::Json(503, serde_json::to_value(ErrorBody::unavailable(m)).unwrap()));
    }
    Ok(store.cfg.models.iter().find(|f| f.name == m.model).cloned().expect("identity lists the config's models"))
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
    json!({"spec_id": id, "spec": spec})
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

/// Renders a context and builds it on served model `m`: starts from the longest snapshot of `m`
/// held whose tokens begin it, and holds a snapshot at each boundary and at the end unless `dry_run`.
fn build(store: &mut Store, m: &FakeModel, c: &Context, dry_run: bool) -> Built {
    let r = model::render(&c.tools, c.thinks(), &c.messages, false);
    let mut counts = Vec::new();
    let mut cuts = Vec::new();
    for cut in &r.cuts {
        counts.push(cut.tokens);
        cuts.push((snapshot_id(m, &r.tokens[..cut.tokens], &counts), *cut));
    }
    let head = cuts.last().unwrap().0.clone();
    // The longest held snapshot whose tokens begin the context: a boundary of this context, or
    // the head a generation held after its reply.
    let n = r.end;
    let best = store.snaps.iter().filter(|(_, s)| s.model == m.name && s.tokens.len() <= n && r.tokens.starts_with(&s.tokens)).max_by(|a, b| a.1.tokens.len().cmp(&b.1.tokens.len()).then(b.0.cmp(a.0)));
    let (kept, from) = best.map_or((0, None), |(id, s)| (s.tokens.len(), Some(id.clone())));
    if !dry_run {
        let mut parent = None;
        for (id, cut) in &cuts {
            store.hold(id.clone(), &m.name, &r.tokens[..cut.tokens], parent);
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
    let m = match route(store, &req.model) {
        Ok(m) => m,
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
    let b = build(store, &m, &req.context, dry_run);
    let snapshots = if dry_run { vec![] } else { b.infos(store) };
    let usage = Usage { prompt: n as u64, kept: b.kept as u64, fed: (n - b.kept) as u64, completion: 0, reasoning: 0 };
    Resp::Json(200, json!({"model": m.name, "head": b.head, "usage": usage, "from": b.from, "dry_run": dry_run, "snapshots": snapshots}))
}

fn snapshot_route(store: &Store, id: &str) -> Resp {
    let Some(s) = store.snaps.get(id) else { return not_found(format!("no snapshot {}", id)) };
    let pins: Vec<Value> = store
        .pins
        .iter()
        .filter(|(_, p)| p.snapshots.iter().any(|x| x == id))
        .map(|(o, p)| json!({"owner": o, "expires_in_s": store.expires_in(p.expires)}))
        .collect();
    Resp::Json(200, json!({"id": id, "model": s.model, "tokens": s.tokens.len(), "tier": "host", "bytes": store.own_tokens(id) as u64 * BYTES_PER_TOKEN, "pins": pins}))
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
        return err(413, "too_large", "the snapshots these pins keep are over pin_bytes", Some("pin_bytes"));
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
    let m = match route(store, &req.model) {
        Ok(m) => m,
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
    let mut usage = Usage::default();
    for (ci, c) in req.contexts.iter().enumerate() {
        let n = model::render(&c.tools, c.thinks(), &c.messages, false).end;
        if n + longest >= store.cfg.context_tokens {
            return context_full(n + longest, store.cfg.context_tokens);
        }
        let b = build(store, &m, c, false);
        let base = State::after(m.seed, &b.tokens);
        for (si, suffix) in suffixes.iter().enumerate() {
            // The first suffix feeds the context past what was held; each read feeds its suffix.
            let kept = if si == 0 { b.kept } else { n };
            usage.prompt += (n + suffix.len()) as u64;
            usage.kept += kept as u64;
            usage.fed += (n + suffix.len() - kept) as u64;
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
    Resp::Json(200, json!({"model": m.name, "reads": out, "signals": {"control_text": control}, "usage": usage}))
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
    let m = match route(store, &req.model) {
        Ok(m) => m,
        Err(r) => return r,
    };
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let limit = store.cfg.context_tokens;
    let max_tokens = req.max_tokens as usize;
    if max_tokens > limit {
        return bad_param("max_tokens", format!("max_tokens is 1 to {}", limit));
    }
    let c = &req.context;
    let thinking = c.thinks();
    let prompt = model::render(&c.tools, thinking, &c.messages, true);
    if prompt.tokens.len() + 1 >= limit {
        return context_full(prompt.tokens.len(), limit);
    }
    let stream = req.stream.unwrap_or(false);
    let last = c.messages.last().and_then(|m| m.content.as_deref()).unwrap_or("");
    let fail = last.contains("FAIL");
    if fail && !stream {
        return err(500, "internal", "nothing was returned: the fake fails a reply whose context ends with a message containing FAIL", None);
    }
    let b = build(store, &m, c, false);
    let sample = req.sample.clone().unwrap_or_default();
    let temperature = sample.temperature.unwrap_or(1.0);
    let seed = sample.seed.unwrap_or_else(|| u64::from_le_bytes(sha256_hex(&prompt.tokens.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>()).as_bytes()[..8].try_into().unwrap()));
    let mut rng = Rng(seed);
    let trigger = !c.tools.is_empty() && last.contains("CALL");

    let mut toks = prompt.tokens.clone();
    let mut st = State::after(m.seed, &toks);
    let mut phase = if thinking { Phase::Think } else { Phase::Answer };
    let (mut reasoning, mut content): (Vec<u8>, Vec<u8>) = (vec![], vec![]);
    let mut forced: VecDeque<u32> = VecDeque::new();
    let mut queued = false;
    let mut in_call = false;
    let mut calls: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut events = vec![StreamEvent::Ping];
    let mut ended = false;
    let mut n = 0;
    let mut thought = 0;
    let mut shown = (0, 0);
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
                EventPhase::Call
            }
            (_, model::ENDCALL) => {
                in_call = false;
                EventPhase::Call
            }
            _ if in_call => EventPhase::Call,
            (Phase::Think, _) => EventPhase::Reasoning,
            _ => EventPhase::Content,
        };
        if kind == EventPhase::Reasoning {
            thought += 1;
        }
        // A reasoning or content token shows the text it adds to its part, so a part's pieces join
        // to that part in `done`; a marker in either shows nothing.
        let piece = match (kind, tok) {
            (EventPhase::Reasoning, t) if t < 256 => {
                reasoning.push(t as u8);
                next_piece(&reasoning, &mut shown.0)
            }
            (EventPhase::Content, t) if t < 256 => {
                content.push(t as u8);
                next_piece(&content, &mut shown.1)
            }
            (EventPhase::Call, t) => model::piece(t),
            _ => String::new(),
        };
        events.push(StreamEvent::Token(TokenEvent { id: tok, piece, phase: kind, p, h, n: n as u64, tps: None }));
        if fail && n == 2 {
            let e = ErrorBody::new(ErrorType::Internal, "the reply stopped: the fake fails a reply whose context ends with a message containing FAIL");
            events.push(StreamEvent::Error(e));
            return Resp::Sse(events);
        }
        match (phase, tok) {
            (Phase::Think, model::ENDTHINK) => phase = Phase::Answer,
            (_, model::END) => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    let finish = match (ended, calls.is_empty()) {
        (true, true) => "end_turn",
        (true, false) => "tool_use",
        _ if toks.len() + 1 >= limit => "context_full",
        _ => "max_tokens",
    };
    let (reasoning, content) = (String::from_utf8_lossy(&reasoning).into_owned(), String::from_utf8_lossy(&content).into_owned());
    let tool_calls: Vec<ToolCall> = calls.into_iter().enumerate().map(|(k, (name, input))| ToolCall { id: format!("call_{}_{}", prompt.tokens.len(), k), name, input }).collect();
    let reply = Message {
        reasoning: Some(reasoning.clone()).filter(|r| !r.is_empty()),
        tool_calls: tool_calls.clone(),
        ..Message::assistant(content.clone())
    };
    let reply = Message { content: Some(content.clone()).filter(|c| !c.is_empty()), ..reply };
    let mut messages = c.messages.clone();
    messages.push(reply);
    let after = Context { messages, ..c.clone() };
    let held = build(store, &m, &after, false);
    let usage = Usage { prompt: prompt.tokens.len() as u64, kept: b.kept as u64, fed: (prompt.tokens.len() - b.kept) as u64, completion: n as u64, reasoning: thought };
    let done = json!({
        "content": content, "reasoning": reasoning,
        "tool_calls": tool_calls.iter().map(|t| json!({"id": t.id, "name": t.name, "input": t.input})).collect::<Vec<_>>(),
        "finish": finish,
        "usage": usage,
        "head": held.head, "snapshots": held.infos(store), "seed": seed, "ms": {"prefill": 0.0, "decode": 0.0}, "model": m.name,
    });
    if stream {
        events.push(StreamEvent::Done(Box::new(serde_json::from_value(done).expect("the fake's done decodes"))));
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
    let m = match route(store, &req.model) {
        Ok(m) => m,
        Err(r) => return r,
    };
    if let Some(t) = &req.think {
        if t.tokens > 0 {
            return bad_param("think", "think above 0 tokens needs the think capability, which this model does not have");
        }
    }
    if let Err(e) = req.validate() {
        return invalid(e);
    }
    let spec_id = req.spec_id.clone();
    let Some(v) = store.specs.get(&spec_id) else { return not_found(format!("no held spec {}; POST it again", spec_id)) };
    let (questions, instructions, label) = match parse_spec(v, &rules()) {
        Ok(s) => (s.questions, s.instructions, s.input_label),
        Err(e) => return invalid(e),
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
    let case_text = match &req.case {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if case_text.len() > CASE_BYTES {
        return err(413, "too_large", format!("nothing was read: the case is {} bytes; case_bytes is {}", case_text.len(), CASE_BYTES), Some("case_bytes"));
    }
    let pool = req.pool.clone().unwrap_or_default();
    let method = pool.method.unwrap_or(PoolMethod::Linear);
    let weights_kind = pool.weights.unwrap_or(PoolWeights::Uniform);
    let ask = Asked { questions: &questions, asked, chosen };
    let mut control: Vec<Value> = Vec::new();
    for (where_, text) in [("case", case_text.as_str()), ("spec", instructions.as_str())] {
        control.extend(model::control_hits(text).into_iter().map(|t| json!({"where": where_, "token": t})));
    }

    // Each read: the context's head, then the answers' log probabilities by asked question.
    let mut reads: Vec<(Option<String>, Vec<Vec<f64>>, usize, String)> = Vec::new();
    let mut usage = Usage::default();
    let contexts: Vec<Option<&Context>> = if req.contexts.is_empty() { vec![None] } else { req.contexts.iter().map(Some).collect() };
    for c in contexts {
        // A read keeps what its build kept and feeds the rest of the context, then everything after it.
        let (head, base_tokens, queue, kept) = match c {
            None => (None, vec![model::BOS], None, 0),
            Some(c) => {
                let n = model::render(&c.tools, c.thinks(), &c.messages, false).end;
                if n > store.cfg.decision_context_tokens {
                    return err(413, "too_large", format!("context is {} tokens; decisions read contexts of up to {}", n, store.cfg.decision_context_tokens), Some("decision_context_tokens"));
                }
                let b = build(store, &m, c, false);
                (Some(b.head), b.tokens, queue_call(c).map(|call| (c, call.id.clone())), b.kept)
            }
        };
        let mut fed = base_tokens.len() - kept;
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
            messages.push(Message::tool(call_id, case_text.clone()));
            let mut toks = model::render(&c.tools, c.thinks(), &messages, true).tokens;
            model::push_text(&mut toks, &format!("call:{}{{answers:{{", c.queue.as_deref().unwrap_or("")));
            for &qi in &sorted {
                let q = &ask.questions[qi];
                model::push_text(&mut toks, &format!("{}:\"", q.id));
                let lp = model::log_softmax(&State::after(m.seed, &toks).logits());
                let read: Vec<f64> = ask.chosen[qi].iter().map(|&j| lp[usize::from(b'A') + j]).collect();
                let best = (0..read.len()).fold(0, |b, i| if read[i] > read[b] { i } else { b });
                model::push_text(&mut toks, &format!("{}\",", (b'A' + ask.chosen[qi][best] as u8) as char));
                lps[qi] = Some(read);
            }
            rendered = sha256_hex(toks.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>().as_slice());
            fed += toks.len() - base_tokens.len();
        } else {
            let mut spec_and_case = Vec::new();
            model::push_text(&mut spec_and_case, &format!("{}\n{}: {}", instructions, label, case_text));
            let mut text_all = base_tokens.clone();
            text_all.push(model::USER);
            text_all.extend(&spec_and_case);
            text_all.extend([model::END, model::ASST]);
            for &qi in &ask.asked {
                let q = &ask.questions[qi];
                let mut toks = text_all.clone();
                model::push_text(&mut toks, &format!("{} {}:", q.id, q.instructions));
                let lp = model::log_softmax(&State::after(m.seed, &toks).logits());
                lps[qi] = Some(ask.chosen[qi].iter().map(|&j| lp[usize::from(b'A') + j]).collect());
                fed += toks.len() - base_tokens.len();
            }
            rendered = sha256_hex(text_all.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>().as_slice());
        }
        usage.prompt += (kept + fed) as u64;
        usage.kept += kept as u64;
        usage.fed += fed as u64;
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
    let mut identity = identity_of(&m);
    identity["spec_id"] = json!(spec_id);
    Resp::Json(
        200,
        json!({"model": m.name, "answers": answers, "reads": reads_json,
               "pool": {"method": method, "weights": weights_kind, "normalized": normalized},
               "signals": {"control_text": control}, "identity": identity,
               "usage": usage,
               "queue_ms": 0.0, "ms": t0.elapsed().as_secs_f64() * 1000.0}),
    )
}

/// The text `buf` adds past the `shown` bytes of its decoding. An incomplete last character waits
/// for a later piece.
fn next_piece(buf: &[u8], shown: &mut usize) -> String {
    let upto = match std::str::from_utf8(buf) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => buf.len(),
    };
    let text = String::from_utf8_lossy(&buf[..upto]);
    let piece = text.get(*shown..).unwrap_or("").to_string();
    *shown = text.len();
    piece
}
