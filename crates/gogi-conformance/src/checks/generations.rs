use gogi_proto::generation::GenerationDone;
use serde_json::json;

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
        let Some(g) = self.generate("generation answers", &ctx, 300) else { return };
        let Some(done) = self.decode::<GenerationDone>("generation reply has the contract's fields", &g) else { return };
        let u = done.usage;
        self.r.check("generation: kept and fed make prompt", u.kept + u.fed == u.prompt && u.prompt > 0, &g["usage"]);
        self.r.check("generation: completion is at most max_tokens, and equals it when the reply was cut", u.completion <= 300 && (g["finish"] != json!("length") || u.completion == 300), &g);
        self.r.check("generation reuses the build", u.kept >= num(&b["tokens"]) as u64, (&g["usage"], &b["tokens"]));
        self.r.check("generation holds its head", g["head"].is_string(), &g["head"]);
        let ids: Vec<&str> = done.tool_calls.iter().map(|c| c.id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        self.r.check("every reply call has its own id", unique.len() == ids.len() && ids.iter().all(|i| !i.is_empty()), &ids);
        self.r.check("generation lists the context's snapshots", g["snapshots"].as_array().is_some_and(|s| !s.is_empty()), &g["snapshots"]);

        // The reply, sent back, is the head the generation reported.
        if g["head"].is_string() {
            let mut with_reply = msgs.clone();
            let cont = self.continue_after(&g, "unused");
            with_reply.push(cont[0].clone());
            let rctx = self.ctx(with_reply);
            if let Some(r) = self.build("the reply appended to its context builds", &rctx, false) {
                self.r.check("the head is the snapshot after the reply as it renders", r["head"] == g["head"] && num(&r["fed"]) == 0, (&r, &g["head"]));
            }
        }

        // The second turn starts from the held head.
        let mut m2 = msgs.clone();
        m2.extend(self.continue_after(&g, "Now list what is left."));
        let c2 = self.ctx(m2);
        if let Some(g2) = self.generate("second turn answers", &c2, 200) {
            let held = g["head"].is_string();
            // A text answer's thinking may be dropped once a user message follows; then the
            // held head is not a prefix of the next request.
            let prefix = done.tool_calls.iter().any(|c| matches!(c, gogi_proto::generation::ReplyToolCall::Parsed { .. })) || g["reasoning"].as_str().is_none_or(str::is_empty);
            if prefix {
                self.r.check("second turn starts from the held head", !held || num(&g2["usage"]["kept"]) + 2 >= (u.prompt + u.completion) as i64, (&g2["usage"], &g["usage"]));
            } else {
                self.r.skip("second turn starts from the held head", "the reply is a text answer with thinking, which the template may drop once a user message follows");
            }
        }

        // With thinking off a reply has no thinking to drop, so its head is a prefix of the next request.
        let mut quiet = ctx.clone();
        quiet["thinking"] = json!(false);
        if let Some(q) = self.generate("generation with thinking off answers", &quiet, 100) {
            if q["head"].is_string() && q["reasoning"].as_str().is_none_or(str::is_empty) {
                let mut m3 = msgs.clone();
                m3.extend(self.continue_after(&q, "Now list what is left."));
                let mut c3 = self.ctx(m3);
                c3["thinking"] = json!(false);
                if let Some(q2) = self.generate("second turn with thinking off answers", &c3, 50) {
                    let want = (q["usage"]["prompt"].as_i64().unwrap_or(0) + q["usage"]["completion"].as_i64().unwrap_or(0)) - 2;
                    self.r.check("with thinking off the second turn starts from the held head", num(&q2["usage"]["kept"]) >= want, (&q2["usage"], &q["usage"]));
                }
            } else {
                self.r.skip("with thinking off the second turn starts from the held head", "the server held no head, or still wrote thinking");
            }
        }

        // Greedy decoding replays.
        if let Some(g3) = self.generate("greedy generation again answers", &ctx, 300) {
            let same = ["content", "reasoning", "tool_calls", "finish"].iter().all(|k| g3[*k] == g[*k]);
            self.r.check("temperature 0 replays the reply", same, (&g3, &g));
        }

        // A stream sends token events and ends with done.
        let body = json!({"context": ctx, "max_tokens": 300, "sample": {"temperature": 0}, "stream": true});
        match self.http.raw("POST", "/council/v2/generations", Some(&body)) {
            Err(e) => self.r.fail("stream answers", e),
            Ok(reply) => {
                self.r.check("stream answers with an event stream", reply.status == 200 && reply.content_type.starts_with("text/event-stream"), (reply.status, &reply.content_type));
                let events = sse_events(&reply.text);
                let last = events.last();
                let tokens = events.iter().filter(|(n, _)| n == "token").count();
                self.r.check("stream ends with done after only token events", last.is_some_and(|(n, _)| n == "done") && events.iter().all(|(n, _)| n == "token" || n == "done"), events.iter().map(|(n, _)| n.as_str()).take(5).collect::<Vec<_>>());
                self.r.check("stream sends a token event for each token", tokens as u64 >= u.completion.min(1), (tokens, u.completion));
                if let Some((_, d)) = last {
                    if self.decode::<GenerationDone>("stream done has the contract's fields", d).is_some() {
                        self.r.check("stream done is the reply the plain call gave", ["content", "reasoning", "tool_calls"].iter().all(|k| d[*k] == g[*k]), (d, &g));
                    }
                }
            }
        }

        // A prompt with no room is a 413 before anything runs.
        let limit = self.identity.as_ref().map_or(0, |i| i.limits.context_tokens) as usize;
        let filler = filler_text(limit * 6, &self.nonce);
        let big = self.ctx(vec![json!({"role": "user", "content": filler})]);
        if limit * 7 > 900_000 {
            self.r.skip("a prompt past context_tokens is refused", format!("context_tokens {} needs a body past 1 MiB", limit));
        } else if let Some(e) = self.refused("a prompt past context_tokens is refused", "POST", "/council/v2/generations", Some(json!({"context": big})), 413) {
            self.r.check(
                "context_full names the prompt and a limit within 1 of identity context_tokens",
                e["error"]["type"] == json!("context_full") && num(&e["error"]["prompt_tokens"]) > num(&e["error"]["context_tokens"]) && (num(&e["error"]["context_tokens"]) - limit as i64).abs() <= 1,
                &e,
            );
        }

        self.refused("a generation refuses a bad context", "POST", "/council/v2/generations", Some(json!({"context": self.ctx(vec![json!({"role": "tool", "tool_call_id": "nope", "content": "x"})])})), 400);
        self.refused("a generation refuses max_tokens 0", "POST", "/council/v2/generations", Some(json!({"context": ctx, "max_tokens": 0})), 400);
        self.refused("a generation refuses a negative temperature", "POST", "/council/v2/generations", Some(json!({"context": ctx, "sample": {"temperature": -1}})), 400);
        self.refused("a generation refuses an unknown member", "POST", "/council/v2/generations", Some(json!({"context": ctx, "from": {}})), 400);
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

