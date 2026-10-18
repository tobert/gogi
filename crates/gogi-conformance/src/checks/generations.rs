use gogi_proto::generation::{Finish, GenerationDone, Phase, StreamEvent};
use serde_json::{Value, json};

use crate::http::sse_events;
use crate::{Suite, num};

impl Suite {
    pub(crate) fn generations(&mut self) {
        if !self.has(gogi_proto::identity::Capability::Generate) {
            self.r.skip("generations", "the server does not declare the generate capability");
            return;
        }
        let msgs = self.opening("generation");
        let ctx = self.ctx(msgs.clone());
        let Some(b) = self.build("build before a generation", &ctx, false) else { return };
        let Some(g) = self.generate("generation succeeds", &ctx, 300) else { return };
        let Some(done) = self.decode::<GenerationDone>("generation response has the contract's fields", &g) else { return };
        let u = done.usage;
        self.r.check("generation usage: kept and fed make prompt, and reasoning is part of completion", u.check().is_ok() && u.prompt > 0, &g["usage"]);
        self.check_finish("generation", &done, 300);
        self.r.check("generation reuses the build", u.kept >= num(&b["usage"]["prompt"]) as u64, (&g["usage"], &b["usage"]));
        self.r.check("generation reports the seed it sampled with", done.seed.is_some(), &g["seed"]);
        let parts: Vec<&Value> = [&g["stats"]["reasoning"], &g["stats"]["content"]].into_iter().chain(g["tool_calls"].as_array().into_iter().flatten().map(|c| &c["stats"])).collect();
        if g.get("stats").is_some() {
            self.r.check("reply stats are well formed where reported", parts.iter().all(|p| well_formed_stats(p)), &parts);
        } else {
            self.r.skip("reply stats are well formed where reported", "the server reports no stats");
        }
        self.r.check("generation holds its head", g["head"].is_string(), &g["head"]);
        let ids: Vec<&str> = done.tool_calls.iter().map(|c| c.id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        self.r.check("every reply call has its own id", unique.len() == ids.len() && ids.iter().all(|i| !i.is_empty()), &ids);
        self.r.check("generation lists the context's snapshots", g["snapshots"].as_array().is_some_and(|s| !s.is_empty()), &g["snapshots"]);

        // The context with the reply appended builds to the head the generation reported.
        if g["head"].is_string() {
            let mut with_reply = msgs.clone();
            let cont = self.continue_after(&g, "unused");
            with_reply.push(cont[0].clone());
            let rctx = self.ctx(with_reply);
            if let Some(r) = self.build("the reply appended to its context builds", &rctx, false) {
                self.r.check("the head is the snapshot after the reply as it renders", r["head"] == g["head"] && num(&r["usage"]["fed"]) == 0, (&r, &g["head"]));
            }
        }

        // A reply and a new user message start from the held head.
        let mut m2 = msgs.clone();
        m2.extend(self.continue_after(&g, "Now list what is left."));
        let c2 = self.ctx(m2);
        if let Some(g2) = self.generate("a reply and a new user message succeed", &c2, 200) {
            let held = g["head"].is_string();
            // The reasoning of a reply without tool calls may be dropped once a user message
            // follows; then the held head is not a prefix of the next request.
            let prefix = done.tool_calls.iter().any(|c| matches!(c, gogi_proto::generation::ReplyToolCall::Parsed { .. })) || g["reasoning"].as_str().is_none_or(str::is_empty);
            if prefix {
                self.r.check("a reply and a new user message start from the held head", !held || num(&g2["usage"]["kept"]) >= head_tokens(&g), (&g2["usage"], &g["head"], &g["snapshots"]));
            } else {
                self.r.skip("a reply and a new user message start from the held head", "the reply has reasoning and no tool calls, and the template may drop that reasoning once a user message follows");
            }
        }

        // With thinking off a reply has no thinking to drop, so its head is a prefix of the next request.
        let mut quiet = ctx.clone();
        quiet["thinking"] = json!(false);
        if let Some(q) = self.generate("generation with thinking off succeeds", &quiet, 100) {
            if q["head"].is_string() && q["reasoning"].as_str().is_none_or(str::is_empty) {
                let mut m3 = msgs.clone();
                m3.extend(self.continue_after(&q, "Now list what is left."));
                let mut c3 = self.ctx(m3);
                c3["thinking"] = json!(false);
                if let Some(q2) = self.generate("with thinking off, a reply and a new user message succeed", &c3, 50) {
                    self.r.check("with thinking off, a reply and a new user message start from the held head", num(&q2["usage"]["kept"]) >= head_tokens(&q), (&q2["usage"], &q["head"], &q["snapshots"]));
                }
            } else {
                self.r.skip("with thinking off, a reply and a new user message start from the held head", "the server held no head, or still wrote thinking");
            }
        }

        // Greedy decoding replays.
        if let Some(g3) = self.generate("greedy generation again succeeds", &ctx, 300) {
            let same = ["content", "reasoning", "tool_calls", "finish"].iter().all(|k| g3[*k] == g[*k]);
            self.r.check("temperature 0 replays the reply", same, (&g3, &g));
        }

        // A stream sends token events and ends with done.
        let body = json!({"context": ctx, "max_tokens": 300, "sample": {"temperature": 0}, "stream": true});
        let body = self.routed("POST", "/council/v2/generations", Some(body)).unwrap_or_default();
        match self.http.raw("POST", "/council/v2/generations", Some(&body)) {
            Err(e) => self.r.fail("stream succeeds", e),
            Ok(response) => {
                self.note("POST", "/council/v2/generations", &response);
                self.r.check("stream returns an event stream", response.status == 200 && response.content_type.starts_with("text/event-stream"), (response.status, &response.content_type));
                let raw = sse_events(&response.text);
                let mut events = Vec::new();
                let (mut unknown, mut faults) = (Vec::new(), Vec::new());
                for (name, data) in &raw {
                    match StreamEvent::parse(name, data) {
                        Ok(Some(e)) => events.push(e),
                        Ok(None) => unknown.push(name.clone()),
                        Err(e) => faults.push(e),
                    }
                }
                self.r.check("stream events decode as the contract's events", faults.is_empty(), &faults);
                self.r.ok_with("stream events a client ignores", format!("{} events with names the contract does not define {:?}", unknown.len(), unknown.iter().take(3).collect::<Vec<_>>()));
                let ends = events.iter().filter(|e| matches!(e, StreamEvent::Done(_) | StreamEvent::Error(_))).count();
                let last_ends = matches!(events.last(), Some(StreamEvent::Done(_) | StreamEvent::Error(_)));
                self.r.check("stream ends with exactly one done or error, and nothing after it", ends == 1 && last_ends && raw.last().is_some_and(|(n, _)| n == "done" || n == "error"), raw.iter().map(|(n, _)| n.as_str()).rev().take(5).collect::<Vec<_>>());
                let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t) } else { None }).collect();
                self.r.check("stream token events count up by 1 from 1", tokens.iter().enumerate().all(|(i, t)| t.n == i as u64 + 1), tokens.iter().map(|t| t.n).take(8).collect::<Vec<_>>());
                self.r.check("stream token events name a phase the contract defines", tokens.iter().all(|t| t.phase != Phase::Unknown), tokens.iter().map(|t| t.phase).find(|p| *p == Phase::Unknown));
                match events.last() {
                    Some(StreamEvent::Done(d)) => {
                        self.r.check("stream sends one token event for each completion token", tokens.len() as u64 == d.usage.completion, (tokens.len(), d.usage));
                        let thought = tokens.iter().filter(|t| t.phase == Phase::Reasoning).count() as u64;
                        self.r.check("stream reasoning token events are usage.reasoning", thought == d.usage.reasoning, (thought, d.usage));
                        let dv = serde_json::to_value(d).unwrap_or_default();
                        self.r.check("the stream's done event equals the response without stream", ["content", "reasoning", "finish"].iter().all(|k| dv[*k] == g[*k]) && without_stats(&dv["tool_calls"]) == without_stats(&g["tool_calls"]) && d.usage.completion == u.completion, (&dv, &g));
                        self.r.check("stream stats match the call without stream within 1e-4", stats_close(&dv, &g, 1e-4), (&dv["stats"], &g["stats"]));
                    }
                    Some(StreamEvent::Error(e)) => {
                        let ok = e.request_id.is_some() && e.request_id == response.request_id;
                        self.r.check("a stream's error event repeats the request-id", ok, (&e.request_id, &response.request_id));
                        self.r.fail("the stream's done event equals the response without stream", format!("the stream ended with an error: {}", e.error.message));
                    }
                    _ => {}
                }
            }
        }

        // A prompt over context_tokens is a 413 context_full, returned before anything runs.
        let limit = self.limits().map_or(0, |l| l.context_tokens) as usize;
        let filler = filler_text(limit * 6, &self.nonce);
        let big = self.ctx(vec![json!({"role": "user", "content": filler})]);
        if limit * 7 > 900_000 {
            self.r.skip("a prompt past context_tokens is refused", format!("context_tokens {} needs a body past 1 MiB", limit));
        } else if let Some(e) = self.refused("a prompt past context_tokens is refused", "POST", "/council/v2/generations", Some(json!({"context": big, "max_tokens": 8})), 413) {
            self.r.check(
                "context_full names the prompt and a limit within 1 of identity context_tokens",
                e["error"]["type"] == json!("context_full") && num(&e["error"]["prompt_tokens"]) > num(&e["error"]["context_tokens"]) && (num(&e["error"]["context_tokens"]) - limit as i64).abs() <= 1,
                &e,
            );
        }

        self.refused("a generation refuses a bad context", "POST", "/council/v2/generations", Some(json!({"context": self.ctx(vec![json!({"role": "tool", "tool_call_id": "nope", "content": "x"})]), "max_tokens": 8})), 400);
        self.refused("a generation refuses max_tokens 0", "POST", "/council/v2/generations", Some(json!({"context": ctx, "max_tokens": 0})), 400);
        if let Some(e) = self.refused("a generation without max_tokens is refused", "POST", "/council/v2/generations", Some(json!({"context": ctx})), 400) {
            self.r.check("a generation without max_tokens is a 400 naming max_tokens", e["error"]["param"] == "max_tokens" || e["error"]["message"].as_str().is_some_and(|m| m.contains("max_tokens")), &e);
        }
        let over = self.limits().map_or(0, |l| l.context_tokens) + 1;
        self.refused("a generation refuses max_tokens past context_tokens", "POST", "/council/v2/generations", Some(json!({"context": ctx, "max_tokens": over})), 400);
        self.refused("a generation refuses a negative temperature", "POST", "/council/v2/generations", Some(json!({"context": ctx, "max_tokens": 8, "sample": {"temperature": -1}})), 400);
        self.refused("a generation refuses an unknown member", "POST", "/council/v2/generations", Some(json!({"context": ctx, "max_tokens": 8, "from": {}})), 400);

        // A reply cut off by max_tokens says so.
        if let Some(short) = self.generate("a generation of 1 token succeeds", &ctx, 1) {
            if let Some(d) = self.decode::<GenerationDone>("a generation of 1 token decodes", &short) {
                self.check_finish("a generation of 1 token", &d, 1);
                self.r.check("a generation of 1 token ends at max_tokens or with its turn", d.usage.completion <= 1 && matches!(d.finish, Finish::MaxTokens | Finish::EndTurn | Finish::ToolUse), &short);
            }
        }
    }

    /// `finish` agrees with the reply: `tool_use` when it ends after calls and `end_turn` when it
    /// ends without them; `max_tokens` when it wrote `max_tokens`; `context_full` when prompt and
    /// reply reach `context_tokens`, within 1.
    pub(crate) fn check_finish(&mut self, what: &str, d: &GenerationDone, max_tokens: u64) {
        let u = d.usage;
        let limit = self.limits().map_or(0, |l| l.context_tokens);
        let ok = match d.finish {
            Finish::ToolUse => !d.tool_calls.is_empty(),
            Finish::EndTurn => d.tool_calls.is_empty(),
            Finish::MaxTokens => u.completion == max_tokens,
            Finish::ContextFull => u.prompt + u.completion + 1 >= limit,
        };
        self.r.check(&format!("{}: finish agrees with the reply", what), ok && u.completion <= max_tokens, (d.finish, d.tool_calls.len(), u, max_tokens, limit));
    }
}

/// Text of `n` letters in a pseudo-random order, so that no tokenizer packs it into fewer than
/// about `n / 6` tokens.
pub(crate) fn filler_text(n: usize, seed: &str) -> String {
    let mut x = seed.bytes().fold(0x9E3779B97F4A7C15u64, |a, b| a.wrapping_mul(0x100000001b3) ^ u64::from(b));
    let mut out = String::with_capacity(n + n / 5);
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((b'a' + (x % 26) as u8) as char);
        if i % 5 == 4 {
            out.push(' ');
        }
    }
    out
}

/// The token count of the head a generation reported, from its `snapshots`; how many tokens the
/// reply renders to is the server's, so it is taken from `snapshots`, not computed from `usage`.
fn head_tokens(g: &Value) -> i64 {
    g["snapshots"].as_array().and_then(|s| s.iter().find(|x| x["id"] == g["head"])).map_or(i64::MAX, |x| num(&x["tokens"]))
}

/// A reply's tool calls without their `stats`, which are diagnostics: a server may compute them
/// from logits whose last bits depend on how the prompt was fed.
fn without_stats(calls: &Value) -> Value {
    let mut c = calls.clone();
    if let Some(a) = c.as_array_mut() {
        for call in a {
            if let Some(o) = call.as_object_mut() {
                o.remove("stats");
            }
        }
    }
    c
}

/// Whether two replies' `stats`, and their calls' `stats`, agree within `tol`; replies without
/// stats agree.
fn stats_close(a: &Value, b: &Value, tol: f64) -> bool {
    fn close(x: &Value, y: &Value, tol: f64) -> bool {
        match (x, y) {
            (Value::Number(p), Value::Number(q)) => (p.as_f64().unwrap_or(f64::NAN) - q.as_f64().unwrap_or(f64::NAN)).abs() <= tol,
            (Value::Object(p), Value::Object(q)) => p.len() == q.len() && p.iter().all(|(k, v)| q.get(k).is_some_and(|w| close(v, w, tol))),
            (Value::Array(p), Value::Array(q)) => p.len() == q.len() && p.iter().zip(q).all(|(v, w)| close(v, w, tol)),
            _ => x == y,
        }
    }
    let calls = |v: &Value| -> Value { Value::Array(v["tool_calls"].as_array().map(|a| a.iter().map(|c| c["stats"].clone()).collect()).unwrap_or_default()) };
    close(&a["stats"], &b["stats"], tol) && close(&calls(a), &calls(b), tol)
}

/// Token stats are null, or `tokens` of at least 1 with `min_logprob <= mean_logprob <= 0` and
/// `0 <= mean_entropy <= max_entropy`.
fn well_formed_stats(s: &Value) -> bool {
    if s.is_null() {
        return true;
    }
    let f = |k: &str| s[k].as_f64().unwrap_or(f64::NAN);
    s["tokens"].as_u64().is_some_and(|n| n >= 1)
        && f("min_logprob") <= f("mean_logprob") + 1e-12
        && f("mean_logprob") <= 1e-12
        && f("mean_entropy") >= -1e-12
        && f("mean_entropy") <= f("max_entropy") + 1e-12
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Null stats pass; stats with no tokens, a minimum above the mean, a positive mean log
    /// probability, or a mean entropy above the maximum fail.
    #[test]
    fn stats_checks_can_fail() {
        let ok = json!({"tokens": 3, "mean_logprob": -0.1, "min_logprob": -0.5, "mean_entropy": 0.2, "max_entropy": 0.9});
        assert!(well_formed_stats(&Value::Null) && well_formed_stats(&ok));
        for (k, v) in [("tokens", json!(0)), ("min_logprob", json!(-0.01)), ("mean_logprob", json!(0.2)), ("mean_entropy", json!(1.5))] {
            let mut bad = ok.clone();
            bad[k] = v;
            assert!(!well_formed_stats(&bad), "{}", bad);
        }
        let a = json!({"stats": {"reasoning": ok, "content": null}, "tool_calls": [{"stats": ok}]});
        let mut b = a.clone();
        b["tool_calls"][0]["stats"]["mean_logprob"] = json!(-0.1 + 1e-9);
        assert!(stats_close(&a, &b, 1e-4));
        b["tool_calls"][0]["stats"]["mean_logprob"] = json!(-0.2);
        assert!(!stats_close(&a, &b, 1e-4));
    }
}
