//! Continuations (capability `continuation`): a generation whose last message is open, so the
//! model goes on from text the client wrote. The checks never assume a template: a cut reply is
//! sent back as the server returned it, and an open text the suite writes may be refused at its
//! seam, which is then checked to name `continue`.

use gogi_proto::generation::{Finish, GenerationDone, Phase, StreamEvent, check_pieces_after};
use gogi_proto::identity::Capability;
use gogi_proto::message::Message;
use serde_json::{Value, json};

use crate::http::sse_events;
use crate::{Suite, num};

/// The most tokens of the reply the checks cut and resume.
const CUT: u64 = 8;
/// The `max_tokens` of the whole reply the checks cut.
const WHOLE: u32 = 300;
/// The `max_tokens` of a continuation.
const RESUME: u32 = 40;

impl Suite {
    pub(crate) fn continuations(&mut self) {
        if !self.has(Capability::Generate) {
            self.r.skip("continuations", "the server does not declare the generate capability");
            return;
        }
        if !self.has(Capability::Continuation) {
            self.r.skip("continuations", "the served model does not declare the continuation capability");
            let open = self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": true}), Some(false));
            if let Some(e) = self.refused("without continuation, an open message is refused", "POST", "/council/v2/generations", Some(json!({"context": open, "max_tokens": 4})), 400) {
                self.r.check("without continuation, an open message is a 400 naming continue", names_continue(&e), &e);
            }
            return;
        }
        let open = self.resume_a_cut_reply();
        self.open_text_the_client_wrote();
        self.refusals();
        self.riding(open);
    }

    /// The opening messages with `last` after them, as a context with `thinking` as given (absent
    /// when `None`).
    fn open_ctx(&self, last: Value, thinking: Option<bool>) -> Value {
        let mut messages = self.opening("continuation");
        messages.push(last);
        let mut c = self.ctx(messages);
        match thinking {
            Some(t) => c["thinking"] = json!(t),
            None => {
                c.as_object_mut().map(|o| o.remove("thinking"));
            }
        }
        c
    }

    /// Cuts a greedy reply before its end, sends it back open, and checks the continuation.
    /// Returns the open message it sent, when the reply could be continued.
    fn resume_a_cut_reply(&mut self) -> Option<Message> {
        let opening = self.opening("continuation");
        let ctx = self.ctx(opening.clone());
        let w = self.generate("a reply to cut succeeds", &ctx, WHOLE)?;
        let whole = self.decode::<GenerationDone>("a reply to cut decodes", &w)?;
        self.close_the_thinking(&opening, &whole);
        if whole.usage.completion < 2 {
            self.r.skip("a cut reply resumes", format!("the whole reply has {} token, too few to cut", whole.usage.completion));
            return None;
        }
        // Greedy replays, so the same request with a smaller max_tokens is cut before the end.
        let k = CUT.min(whole.usage.completion - 1) as u32;
        let g = self.generate("a cut reply succeeds", &ctx, k)?;
        let cut = self.decode::<GenerationDone>("a cut reply decodes", &g)?;
        if cut.finish != Finish::MaxTokens {
            self.r.fail("a cut reply resumes", format!("the reply ended with {:?} within {} tokens, though the same request wrote {} tokens with a larger max_tokens", cut.finish, k, whole.usage.completion));
            return None;
        }
        self.r.check("a cut reply names the phase of its next token", cut.phase.is_some_and(|p| p != Phase::Unknown), &g["phase"]);
        let head_tokens = match &cut.head {
            Some(h) => {
                let last = cut.snapshots.last();
                self.r.check("a cut reply's head is listed last in snapshots", last.is_some_and(|s| &s.id == h), (&g["head"], &g["snapshots"]));
                last.map(|s| s.tokens)
            }
            None => {
                self.r.skip("a cut reply's head is listed last in snapshots", "the server held no head for the cut reply");
                None
            }
        };
        let Some(open) = cut.as_open_message() else {
            self.r.skip("a cut reply resumes", format!("a reply cut in phase {:?} with tool calls {} cannot be continued", cut.phase, cut.tool_calls.len()));
            return None;
        };
        let mut messages = opening.clone();
        messages.push(serde_json::to_value(&open).unwrap_or_default());
        let octx = self.ctx(messages);

        if let Some(d) = self.build("a dry run of a cut reply sent back open succeeds", &octx, true) {
            let from_head = match &cut.head {
                Some(h) => d["from"] == json!(h) && num(&d["usage"]["fed"]) <= 1,
                None => true,
            };
            self.r.check("a dry run of a cut reply sent back open starts from its head", d["dry_run"] == json!(true) && from_head, (&d, &cut.head));
        }
        if let Some(e) = self.refused("a build without dry_run refuses an open message", "POST", "/council/v2/contexts", Some(json!({"context": octx})), 400) {
            self.r.check("a build without dry_run refuses an open message, naming continue", names_continue(&e), &e);
        }

        let g2 = self.generate("a continuation of a cut reply succeeds", &octx, RESUME)?;
        let d2 = self.decode::<GenerationDone>("a continuation decodes", &g2)?;
        let starts = d2.continues(&open);
        self.r.check("a continuation's done starts with the open text", starts.is_ok(), starts.err());
        let u = d2.usage;
        match head_tokens {
            Some(t) => {
                self.r.check("a continuation resumed from the head feeds at most the last sampled token", u.check().is_ok() && u.prompt == t && u.fed <= 1, (u, t));
            }
            None => self.r.skip("a continuation resumed from the head feeds at most the last sampled token", "the server held no head for the cut reply"),
        }
        self.r.check("a continuation's completion counts only the sampled tokens", u.completion <= RESUME as u64 && u.reasoning <= u.completion && u.prompt > cut.usage.prompt, (u, cut.usage));
        self.check_finish("a continuation", &d2, RESUME as u64);
        self.stream_a_continuation(&octx, &open, &d2);
        Some(open)
    }

    /// Streams the continuation of `octx` and checks its token events against `done`.
    fn stream_a_continuation(&mut self, octx: &Value, open: &Message, unstreamed: &GenerationDone) {
        let body = self.routed("POST", "/council/v2/generations", Some(json!({"context": octx, "max_tokens": RESUME, "sample": {"temperature": 0}, "stream": true}))).unwrap_or_default();
        let response = match self.http.raw("POST", "/council/v2/generations", Some(&body)) {
            Ok(r) => r,
            Err(e) => return self.r.fail("a continuation's stream succeeds", e),
        };
        self.note("POST", "/council/v2/generations", &response);
        let events: Vec<StreamEvent> = sse_events(&response.text).iter().filter_map(|(n, d)| StreamEvent::parse(n, d).ok().flatten()).collect();
        let Some(StreamEvent::Done(done)) = events.last() else {
            return self.r.fail("a continuation's stream succeeds", format!("returned {} without a done event: {}", response.status, response.text));
        };
        let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t.clone()) } else { None }).collect();
        self.r.check("a continuation's stream counts n from 1 over the sampled tokens", tokens.iter().enumerate().all(|(i, t)| t.n == i as u64 + 1), tokens.iter().map(|t| t.n).take(8).collect::<Vec<_>>());
        let thought = tokens.iter().filter(|t| t.phase == Phase::Reasoning).count() as u64;
        self.r.check("a continuation's stream sends one token event for each completion token", tokens.len() as u64 == done.usage.completion && thought == done.usage.reasoning, (tokens.len(), thought, done.usage));
        let joined = check_pieces_after(&tokens, done, open);
        self.r.check("a continuation's pieces join to done's text after the open text", joined.is_ok(), joined.err());
        self.r.check("a continuation's stream ends with the reply without stream", (&done.content, &done.reasoning, done.finish) == (&unstreamed.content, &unstreamed.reasoning, unstreamed.finish), (&done.content, &unstreamed.content));
    }

    /// Sends a reply's reasoning back open with `content: ""`, which closes the thinking.
    fn close_the_thinking(&mut self, opening: &[Value], reply: &GenerationDone) {
        if reply.reasoning.is_empty() {
            self.r.skip("an empty content after reasoning closes the thinking", "the reply wrote no reasoning");
            return;
        }
        let open = json!({"role": "assistant", "reasoning": reply.reasoning, "content": "", "continue": true});
        let mut messages = opening.to_vec();
        messages.push(open);
        let Some(g) = self.generate("a continuation with an empty content succeeds", &self.ctx(messages), RESUME) else { return };
        self.r.check("an empty content after reasoning closes the thinking", g["reasoning"] == json!(reply.reasoning) && num(&g["usage"]["reasoning"]) == 0, &g);
    }

    /// Open text the suite writes: the server continues it, or refuses it at its seam, naming
    /// `continue`.
    fn open_text_the_client_wrote(&mut self) {
        let plan = self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": true}), Some(false));
        if let Some((status, v)) = self.send("a continuation of open content the client wrote", "POST", "/council/v2/generations", Some(json!({"context": plan, "max_tokens": RESUME, "sample": {"temperature": 0}}))) {
            match status {
                200 => self.r.check("a continuation of open content the client wrote starts with it", v["content"].as_str().is_some_and(|c| c.starts_with("Plan:")) && num(&v["usage"]["reasoning"]) == 0, &v),
                400 => self.r.check("a continuation of open content the client wrote starts with it", names_continue(&v), ("refused at its seam", &v)),
                _ => self.r.check("a continuation of open content the client wrote starts with it", false, (status, &v)),
            };
        }
        if let Some((status, v)) = self.send("a dry run of an open message the client wrote", "POST", "/council/v2/contexts", Some(json!({"context": plan, "dry_run": true}))) {
            let ok = match status {
                200 => v["dry_run"] == json!(true) && serde_json::from_value::<gogi_proto::Usage>(v["usage"].clone()).is_ok_and(|u| u.check().is_ok() && u.completion == 0 && u.prompt > 0),
                400 => names_continue(&v),
                _ => false,
            };
            self.r.check("a dry run of an open message the client wrote reports what it would feed", ok, (status, &v));
        }
    }

    /// Each misuse of `continue` is a `400` naming `continue`.
    fn refusals(&mut self) {
        let generation = |c: Value| json!({"context": c, "max_tokens": 4});
        let mut not_last = self.opening("continuation");
        not_last.push(json!({"role": "assistant", "content": "Plan:", "continue": true}));
        not_last.push(json!({"role": "user", "content": "Go on."}));
        let cases: Vec<(&str, Value)> = vec![
            ("an open message that is not last", self.ctx(not_last)),
            ("an open user message", self.open_ctx(json!({"role": "user", "content": "Go on.", "continue": true}), Some(true))),
            ("an open message with tool_calls", self.open_ctx(json!({"role": "assistant", "content": "Plan:", "tool_calls": [{"id": "c0", "name": "shell", "input": {}}], "continue": true}), Some(true))),
            ("an open message with no text", self.open_ctx(json!({"role": "assistant", "reasoning": "", "continue": true}), Some(true))),
            ("an open message with reasoning and thinking false", self.open_ctx(json!({"role": "assistant", "reasoning": "First I", "continue": true}), Some(false))),
            ("an open message with reasoning and thinking absent", self.open_ctx(json!({"role": "assistant", "reasoning": "First I", "continue": true}), None)),
            ("continue that is a string", self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": "yes"}), Some(true))),
            ("continue that is null", self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": null}), Some(true))),
        ];
        for (name, c) in cases {
            if let Some(e) = self.refused(&format!("{} is refused", name), "POST", "/council/v2/generations", Some(generation(c)), 400) {
                self.r.check(&format!("{} is a 400 naming continue", name), names_continue(&e), &e);
            }
        }
        let open = self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": true}), Some(false));
        if let Some(spec_id) = self.hold_gate("continuation") {
            let d = json!({"spec_id": spec_id, "case": "ls", "contexts": [open]});
            if let Some(e) = self.refused("a decision with an open context is refused", "POST", "/council/v2/decisions", Some(d), 400) {
                self.r.check("a decision with an open context is a 400 naming continue", names_continue(&e), &e);
            }
            self.expect("spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
        }
        if self.has(Capability::Reads) {
            let r = json!({"contexts": [open], "suffixes": ["\nAnswer: "], "letters": ["A", "B"]});
            if let Some(e) = self.refused("raw reads of an open context are refused", "POST", "/council/v2/reads", Some(r), 400) {
                self.r.check("raw reads of an open context are a 400 naming continue", names_continue(&e), &e);
            }
        } else {
            self.r.skip("raw reads of an open context are a 400 naming continue", "the server does not declare the reads capability");
        }

        // continue: false is allowed on any message and changes nothing.
        let plain = self.ctx(self.opening("continue-false"));
        let mut marked = plain.clone();
        if let Some(ms) = marked["messages"].as_array_mut() {
            for m in ms {
                m["continue"] = json!(false);
            }
        }
        if let (Some(a), Some(b)) = (self.build("a context builds", &plain, false), self.build("the same context with continue: false builds", &marked, false)) {
            self.r.check("continue: false on earlier messages changes nothing", a["head"] == b["head"] && num(&b["usage"]["fed"]) == 0, (&a, &b));
        }
    }

    /// A riding `before` reads after the open text, and `after` after the whole message; neither
    /// changes the reply.
    fn riding(&mut self, open: Option<Message>) {
        if !self.has(Capability::Ride) {
            self.r.skip("a riding decision rides with a continuation", "the server does not declare the ride capability");
            return;
        }
        let Some(open) = open else {
            self.r.skip("a riding decision rides with a continuation", "no cut reply could be continued");
            return;
        };
        let Some(spec_id) = self.hold_gate("continuation-ride") else { return };
        let mut messages = self.opening("continuation");
        messages.push(serde_json::to_value(&open).unwrap_or_default());
        let octx = self.ctx(messages);
        let bare = json!({"context": octx, "max_tokens": RESUME, "sample": {"temperature": 0}});
        let mut rode = bare.clone();
        rode["decisions"] = json!([{"spec_id": spec_id, "when": "before"}, {"spec_id": spec_id, "when": "after", "ask": ["verdict"]}]);
        if let (Some(p), Some(g)) = (self.expect("a continuation without decisions succeeds", "POST", "/council/v2/generations", Some(bare), 200), self.expect("a continuation with riding decisions succeeds", "POST", "/council/v2/generations", Some(rode), 200)) {
            let reply = |v: &Value| (v["content"].clone(), v["reasoning"].clone());
            let d = g["decisions"].as_array().cloned().unwrap_or_default();
            let ordered = d.len() == 2 && d[0]["when"] == json!("before") && d[1]["when"] == json!("after");
            self.r.check("a riding decision rides with a continuation", ordered && reply(&p) == reply(&g), (&g["decisions"], reply(&p), reply(&g)));
        }
        self.expect("spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }
}

/// An error body names `continue` in `param`.
fn names_continue(v: &Value) -> bool {
    v["error"]["param"] == json!("continue")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_continue_is_the_param_not_the_message() {
        assert!(names_continue(&json!({"error": {"type": "invalid_request", "message": "m", "param": "continue"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "message": "continue is wrong", "param": "contexts"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "message": "continue is wrong"}})));
    }
}
