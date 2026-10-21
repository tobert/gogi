//! Continuations (capability `continuation`): a generation whose last message is open, so the
//! model goes on from text the client wrote. The checks never assume a template: a cut reply is
//! sent back as the server returned it, and an open text may be refused at its seam, a `400`
//! naming `continue`, which skips the checks that need it.

use gogi_proto::context::ContextBuilt;
use gogi_proto::generation::{Finish, GenerationDone, Phase, StreamEvent, check_pieces_after, trim_edges};
use gogi_proto::identity::Capability;
use gogi_proto::message::Message;
use serde_json::{Value, json};

use super::generations::call_shape;
use super::ride::well_formed;
use crate::http::sse_events;
use crate::{Suite, num};

/// The most tokens of the reply the checks cut and resume.
const CUT: u64 = 8;
/// The `max_tokens` of the whole reply the checks cut.
const WHOLE: u32 = 300;
/// The `max_tokens` of a continuation of text the suite writes.
const RESUME: u32 = 40;

/// What a request returned: the response, a refusal at the seam, or a failure already recorded.
enum Sent {
    Ok(Value),
    Seam,
    Failed,
}

impl Suite {
    pub(crate) fn continuations(&mut self) {
        if !self.has(Capability::Generate) {
            self.r.skip("continuations", "the served model does not declare the generate capability");
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

    /// Sends a request whose open text the server may refuse at its seam. A `400` naming
    /// `continue` skips `what` with that reason; any other status but `200` fails it.
    fn send_open(&mut self, what: &str, path: &str, body: Value) -> Sent {
        let Some((status, v)) = self.send(what, "POST", path, Some(body)) else { return Sent::Failed };
        match status {
            200 => Sent::Ok(v),
            400 if names_continue(&v) => {
                self.r.skip(what, format!("the server refused the open text at its seam: {}", v["error"]["message"]));
                Sent::Seam
            }
            _ => {
                self.r.fail(what, format!("POST {} returned {}, not 200 or a 400 naming continue: {}", path, status, v));
                Sent::Failed
            }
        }
    }

    /// Writes a greedy reply, cuts the same request before its end, sends the cut reply back open,
    /// and checks the continuation against the whole reply. Returns the open message it sent, when
    /// the reply could be continued.
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
        // Greedy replays, so the same request with a smaller max_tokens is cut before the end. The
        // whole reply fit, so the cut leaves room in the context.
        let k = CUT.min(whole.usage.completion - 1);
        let g = self.generate("a cut reply succeeds", &ctx, k as u32)?;
        let cut = self.decode::<GenerationDone>("a cut reply decodes", &g)?;
        match cut.finish {
            Finish::MaxTokens => {}
            Finish::ContextFull => {
                self.r.skip("a cut reply resumes", "the cut reply filled the context, and a context_full reply cannot be continued");
                return None;
            }
            other => {
                self.r.fail("a cut reply resumes", format!("the reply ended with {:?} within {} tokens, though the same request wrote {} tokens with a larger max_tokens", other, k, whole.usage.completion));
                return None;
            }
        }
        let prefix = cut.usage.completion == k && whole.reasoning.starts_with(cut.reasoning.trim_end()) && whole.content.starts_with(cut.content.trim_end());
        self.r.check("a cut reply is the start of the whole reply", prefix, (&cut.reasoning, &cut.content, cut.usage, &whole.reasoning, &whole.content));
        self.r.check("a cut reply names the phase of its next token", cut.phase.is_some_and(|p| p != Phase::Unknown), &g["phase"]);
        let agrees = match cut.phase {
            Some(Phase::Reasoning) => cut.content.is_empty() && cut.tool_calls.is_empty(),
            Some(Phase::Content) => trim_edges(&cut.reasoning) == trim_edges(&whole.reasoning),
            Some(Phase::Call) => !whole.tool_calls.is_empty(),
            _ => false,
        };
        self.r.check("a cut reply's phase agrees with its text and the whole reply", agrees, (cut.phase, &cut.reasoning, &cut.content, &whole.reasoning));
        let head_tokens = match &cut.head {
            Some(h) => {
                let last = cut.snapshots.last();
                self.r.check("a cut reply's head is listed last in snapshots", last.is_some_and(|s| &s.id == h), (&g["head"], &g["snapshots"]));
                last.filter(|s| &s.id == h).map(|s| s.tokens)
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
        if let Some(e) = self.refused("a build without dry_run refuses an open message", "POST", "/council/v2/contexts", Some(json!({"context": octx})), 400) {
            self.r.check("a build without dry_run refuses an open message, naming continue", names_continue(&e), &e);
        }

        // Pin the head, so the server keeps it for the checks that start from it.
        let owner = format!("conformance-continuation-{}", self.nonce);
        let pinned = match (&cut.head, self.has(Capability::Pins)) {
            (Some(h), true) => self.expect("the cut reply's head pins", "PUT", &format!("/council/v2/pins/{}", owner), Some(json!({"snapshots": [h], "ttl_s": 600})), 200).is_some(),
            _ => false,
        };
        let resumed = self.resume(&octx, &open, &cut, &whole, k, head_tokens, pinned);
        if pinned {
            self.expect("the cut reply's head unpins", "DELETE", &format!("/council/v2/pins/{}", owner), None, 204);
        }
        resumed.then_some(open)
    }

    /// The checks of a cut reply sent back open as `octx`. False when the server refused it at the
    /// seam.
    #[allow(clippy::too_many_arguments)]
    fn resume(&mut self, octx: &Value, open: &Message, cut: &GenerationDone, whole: &GenerationDone, k: u64, head_tokens: Option<u64>, pinned: bool) -> bool {
        // A head may hold the state before or after the last sampled token, and another snapshot of
        // the same tokens may be chosen, so the checks read `kept` and `fed`, not `from`.
        let from_head = |s: &mut Self, what: &str, kept: u64, fed: u64| match head_tokens {
            Some(t) if kept >= t && fed <= 1 => s.r.ok_with(what, ""),
            Some(t) if !pinned => s.r.skip(what, format!("kept {} of the head's {} tokens; the server may have evicted the head, which it did not pin", kept, t)),
            Some(t) => s.r.fail(what, format!("kept {} and fed {}, though the pinned head holds {} tokens", kept, fed, t)),
            None => s.r.skip(what, "the server held no head for the cut reply"),
        };
        if self.has(Capability::DryRun) {
            match self.send_open("a dry run of a cut reply sent back open succeeds", "/council/v2/contexts", json!({"context": octx, "dry_run": true})) {
                Sent::Ok(d) => {
                    if let Some(b) = self.decode::<ContextBuilt>("a dry run of a cut reply sent back open decodes", &d) {
                        self.r.check("a dry run of a cut reply sent back open reports a dry run", b.dry_run && b.usage.check().is_ok(), &d);
                        from_head(self, "a dry run of a cut reply sent back open keeps its head", b.usage.kept, b.usage.fed);
                    }
                }
                Sent::Seam => return false,
                Sent::Failed => {}
            }
        } else {
            self.r.skip("a dry run of a cut reply sent back open keeps its head", "the served model does not declare the dry_run capability");
        }

        let max_tokens = WHOLE - k as u32;
        let body = json!({"context": octx, "max_tokens": max_tokens, "sample": {"temperature": 0}});
        let g2 = match self.send_open("a continuation of a cut reply succeeds", "/council/v2/generations", body) {
            Sent::Ok(v) => v,
            Sent::Seam => return false,
            Sent::Failed => return true,
        };
        let Some(d2) = self.decode::<GenerationDone>("a continuation decodes", &g2) else { return true };
        let starts = d2.continues(open);
        self.r.check("a continuation's done starts with the open text", starts.is_ok(), starts.err());
        let u = d2.usage;
        self.r.check("a continuation's usage relations hold", u.check().is_ok(), u);
        from_head(self, "a continuation resumed from the head feeds at most the last sampled token", u.kept, u.fed);
        self.r.check("a continuation's completion counts only the sampled tokens", u.completion <= max_tokens as u64 && u.prompt > cut.usage.prompt && u.completion + k <= WHOLE as u64, (u, cut.usage));
        self.check_finish("a continuation", &d2, max_tokens as u64);
        let same = (&d2.reasoning, &d2.content, d2.finish) == (&whole.reasoning, &whole.content, whole.finish) && call_shape(&g2["tool_calls"]) == call_shape(&serde_json::to_value(&whole.tool_calls).unwrap_or_default());
        let begun = d2.finish != whole.finish && whole.reasoning.starts_with(d2.reasoning.trim_end()) && whole.content.starts_with(d2.content.trim_end());
        self.r.check("a continuation writes what the whole reply wrote", same || begun, ((&d2.reasoning, &d2.content, d2.finish), (&whole.reasoning, &whole.content, whole.finish)));
        let phase_ok = match cut.phase {
            Some(Phase::Reasoning) => u.reasoning >= 1,
            _ => u.reasoning == 0,
        };
        self.r.check("a continuation starts in the phase its open message sets", phase_ok, (cut.phase, u));
        if d2.finish == Finish::MaxTokens {
            self.r.check("a continuation that ends with max_tokens names its phase", d2.phase.is_some_and(|p| p != Phase::Unknown), &g2["phase"]);
        }
        match &d2.stats {
            Some(s) => {
                let calls: u64 = g2["tool_calls"].as_array().map(|a| a.iter().filter_map(|c| c["stats"]["tokens"].as_u64()).sum()).unwrap_or(0);
                let tokens = s.reasoning.map_or(0, |x| x.tokens) + s.content.map_or(0, |x| x.tokens) + calls;
                self.r.check("a continuation's stats count only the sampled tokens", tokens <= u.completion, (tokens, u));
            }
            None => self.r.skip("a continuation's stats count only the sampled tokens", "the server reports no stats"),
        }
        self.stream_a_continuation(octx, open, &g2, max_tokens);

        let mut snapped = octx.clone();
        if let Some(last) = snapped["messages"].as_array_mut().and_then(|m| m.last_mut()) {
            last["snap"] = json!(true);
        }
        if let Sent::Ok(v) = self.send_open("snap: true on the open message is allowed", "/council/v2/generations", json!({"context": snapped, "max_tokens": 4, "sample": {"temperature": 0}})) {
            let ok = serde_json::from_value::<GenerationDone>(v.clone()).is_ok_and(|d| d.continues(open).is_ok());
            self.r.check("snap: true on the open message is allowed", ok, &v);
        }
        true
    }

    /// Streams the continuation of `octx` and checks its events and its `done` against the
    /// response without stream, `unstreamed`.
    fn stream_a_continuation(&mut self, octx: &Value, open: &Message, unstreamed: &Value, max_tokens: u32) {
        let body = self.routed("POST", "/council/v2/generations", Some(json!({"context": octx, "max_tokens": max_tokens, "sample": {"temperature": 0}, "stream": true}))).unwrap_or_default();
        let response = match self.http.raw("POST", "/council/v2/generations", Some(&body)) {
            Ok(r) => r,
            Err(e) => return self.r.fail("a continuation's stream succeeds", e),
        };
        self.note("POST", "/council/v2/generations", &response);
        self.r.check("a continuation's stream returns an event stream", response.status == 200 && response.content_type.starts_with("text/event-stream"), (response.status, &response.content_type));
        let raw = sse_events(&response.text);
        let mut events = Vec::new();
        let mut faults = Vec::new();
        for (name, data) in &raw {
            match StreamEvent::parse(name, data) {
                Ok(Some(e)) => events.push(e),
                Ok(None) => {}
                Err(e) => faults.push(e),
            }
        }
        self.r.check("a continuation's stream events decode as the contract's events", faults.is_empty(), &faults);
        let ends = events.iter().filter(|e| matches!(e, StreamEvent::Done(_) | StreamEvent::Error(_))).count();
        let Some(StreamEvent::Done(done)) = events.last().filter(|_| ends == 1) else {
            return self.r.fail("a continuation's stream ends with exactly one done", raw.iter().map(|(n, _)| n.as_str()).rev().take(5).collect::<Vec<_>>().join(", "));
        };
        self.r.ok_with("a continuation's stream ends with exactly one done", "");
        let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t.clone()) } else { None }).collect();
        self.r.check("a continuation's stream counts n from 1 over the sampled tokens", tokens.iter().enumerate().all(|(i, t)| t.n == i as u64 + 1), tokens.iter().map(|t| t.n).take(8).collect::<Vec<_>>());
        self.r.check("a continuation's stream token events name a phase the contract defines", tokens.iter().all(|t| t.phase != Phase::Unknown), tokens.iter().map(|t| t.phase).find(|p| *p == Phase::Unknown));
        let thought = tokens.iter().filter(|t| t.phase == Phase::Reasoning).count() as u64;
        self.r.check("a continuation's stream sends one token event for each completion token", tokens.len() as u64 == done.usage.completion && thought == done.usage.reasoning, (tokens.len(), thought, done.usage));
        self.r.check("a continuation's stream usage relations hold", done.usage.check().is_ok(), done.usage);
        let joined = check_pieces_after(&tokens, done, open);
        self.r.check("a continuation's pieces join to done's text after the open text", joined.is_ok(), joined.err());
        let dv = serde_json::to_value(done).unwrap_or_default();
        let same = ["content", "reasoning", "finish"].iter().all(|k| dv[*k] == unstreamed[*k])
            && call_shape(&dv["tool_calls"]) == call_shape(&unstreamed["tool_calls"])
            && dv["usage"]["completion"] == unstreamed["usage"]["completion"];
        self.r.check("a continuation's stream done equals the response without stream", same, (&dv, unstreamed));
    }

    /// Sends a reply's reasoning back open with `content: ""`, which closes the thinking. When the
    /// reply wrote no reasoning, the suite writes it.
    fn close_the_thinking(&mut self, opening: &[Value], reply: &GenerationDone) {
        let what = "an empty content after reasoning closes the thinking";
        let reasoning = if reply.reasoning.is_empty() { "First I list the files." } else { reply.reasoning.as_str() };
        let open = json!({"role": "assistant", "reasoning": reasoning, "content": "", "continue": true});
        let mut messages = opening.to_vec();
        messages.push(open);
        let body = json!({"context": self.ctx(messages), "max_tokens": RESUME, "sample": {"temperature": 0}});
        if let Sent::Ok(g) = self.send_open(what, "/council/v2/generations", body) {
            self.r.check(what, g["reasoning"] == json!(reasoning) && num(&g["usage"]["reasoning"]) == 0, &g);
        }
    }

    /// Open text the suite writes: the server continues it, or refuses it at its seam, naming
    /// `continue`.
    fn open_text_the_client_wrote(&mut self) {
        let plan = self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": true}), Some(false));
        let what = "a continuation of open content the client wrote starts with it";
        if let Sent::Ok(v) = self.send_open(what, "/council/v2/generations", json!({"context": plan, "max_tokens": RESUME, "sample": {"temperature": 0}})) {
            self.r.check(what, v["content"].as_str().is_some_and(|c| c.starts_with("Plan:")) && num(&v["usage"]["reasoning"]) == 0, &v);
        }
        let absent = self.open_ctx(json!({"role": "assistant", "content": "Plan:", "continue": true}), None);
        let what = "open content with thinking absent is allowed";
        if let Sent::Ok(v) = self.send_open(what, "/council/v2/generations", json!({"context": absent, "max_tokens": 4, "sample": {"temperature": 0}})) {
            self.r.check(what, v["content"].as_str().is_some_and(|c| c.starts_with("Plan:")), &v);
        }
        let what = "a dry run of an open message the client wrote reports what it would feed";
        if !self.has(Capability::DryRun) {
            self.r.skip(what, "the served model does not declare the dry_run capability");
        } else if let Sent::Ok(v) = self.send_open(what, "/council/v2/contexts", json!({"context": plan, "dry_run": true})) {
            let ok = serde_json::from_value::<ContextBuilt>(v.clone()).is_ok_and(|b| b.dry_run && b.usage.check().is_ok() && b.usage.completion == 0 && b.usage.prompt > 0);
            self.r.check(what, ok, &v);
        }
    }

    /// Each misuse of `continue` is a `400` naming `continue`, and `continue: false` changes
    /// nothing.
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
            self.r.skip("raw reads of an open context are a 400 naming continue", "the served model does not declare the reads capability");
        }

        // continue: false is allowed on any message and changes nothing. Snapshot ids may differ
        // between two builds of the same tokens, so the checks compare token counts.
        let plain = self.ctx(self.opening("continue-false"));
        let mut marked = plain.clone();
        if let Some(ms) = marked["messages"].as_array_mut() {
            for m in ms {
                m["continue"] = json!(false);
            }
        }
        if let (Some(a), Some(b)) = (self.build("a context builds", &plain, false), self.build("the same context with continue: false builds", &marked, false)) {
            self.r.check("continue: false on earlier messages changes nothing", a["usage"]["prompt"] == b["usage"]["prompt"], (&a["usage"], &b["usage"]));
        }
        let mut last = self.opening("continue-false-last");
        last.push(json!({"role": "assistant", "content": "Plan: list the files.", "continue": false}));
        last.push(json!({"role": "user", "content": "Go on.", "continue": false}));
        let closed = self.ctx(last.clone());
        if let Some(l) = last.last_mut().and_then(Value::as_object_mut) {
            l.remove("continue");
        }
        let unmarked = self.ctx(last);
        if let (Some(a), Some(b)) = (self.generate("a generation without continue succeeds", &unmarked, 4), self.generate("a generation with continue: false on the last message succeeds", &closed, 4)) {
            self.r.check("continue: false on the last message is a closed message", a["usage"]["prompt"] == b["usage"]["prompt"] && a["content"] == b["content"] && a["reasoning"] == b["reasoning"], (&a, &b));
        }
    }

    /// A riding `before` reads after the open text, and `after` after the whole message; neither
    /// changes the reply.
    fn riding(&mut self, open: Option<Message>) {
        let what = "a riding decision rides with a continuation";
        if !self.has(Capability::Ride) {
            self.r.skip(what, "the served model does not declare the ride capability");
            return;
        }
        let Some(open) = open else {
            self.r.skip(what, "no cut reply could be continued");
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
            let reply = |v: &Value| (v["content"].clone(), v["reasoning"].clone(), call_shape(&v["tool_calls"]));
            self.r.check("riding decisions do not change a continuation", reply(&p) == reply(&g), (reply(&p), reply(&g)));
            let d = g["decisions"].as_array().cloned().unwrap_or_default();
            let ordered = d.len() == 2 && d[0]["when"] == json!("before") && d[1]["when"] == json!("after") && d.iter().all(|x| x["spec_id"] == json!(spec_id));
            let answers: Vec<&Value> = d.iter().flat_map(|x| x["answers"].as_object().into_iter().flat_map(|m| m.values())).collect();
            let keys = |x: &Value| x["answers"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
            let shaped = ordered && keys(&d[0]) == ["verdict", "undo", "risky"] && keys(&d[1]) == ["verdict"] && answers.iter().all(|a| well_formed(a));
            self.r.check(what, shaped, &g["decisions"]);
        }
        self.expect("spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }
}

/// An error body names `continue` in `param` and says why in a `message` that is not empty.
fn names_continue(v: &Value) -> bool {
    v["error"]["param"] == json!("continue") && v["error"]["message"].as_str().is_some_and(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_continue_is_the_param_and_a_message() {
        assert!(names_continue(&json!({"error": {"type": "invalid_request", "message": "m", "param": "continue"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "message": "", "param": "continue"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "param": "continue"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "message": "continue is wrong", "param": "contexts"}})));
        assert!(!names_continue(&json!({"error": {"type": "invalid_request", "message": "continue is wrong"}})));
    }
}
