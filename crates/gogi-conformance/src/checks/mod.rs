//! The checks, grouped by what they test.

mod decisions;
mod generations;
mod pins;
mod reads;
mod restart;
mod ride;

use gogi_proto::context::ContextBuilt;
use gogi_proto::identity::{Capability, ServerIdentity};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{Suite, num};

pub(crate) fn shell_tools() -> Value {
    json!([{"type": "function", "function": {"name": "shell", "description": "Runs a shell command.",
        "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}}])
}

impl Suite {
    pub(crate) fn has(&self, c: Capability) -> bool {
        self.identity.as_ref().is_some_and(|i| i.has(c))
    }

    /// Sends a request and returns its status and body. A transport failure is recorded.
    pub(crate) fn send(&mut self, what: &str, method: &str, path: &str, body: Option<Value>) -> Option<(u16, Value)> {
        match self.http.call(method, path, body.as_ref()) {
            Ok(r) => Some(r),
            Err(e) => {
                self.r.fail(what, e);
                None
            }
        }
    }

    /// Sends a request that must return the status `want`. A different status is recorded as a failure.
    pub(crate) fn expect(&mut self, what: &str, method: &str, path: &str, body: Option<Value>, want: u16) -> Option<Value> {
        let (status, v) = self.send(what, method, path, body)?;
        if status == want {
            Some(v)
        } else {
            self.r.fail(what, format!("{} {} returned {}, not {}: {}", method, path, status, want, v));
            None
        }
    }

    /// Checks that a request is refused with `want` and an error body of the contract's shape.
    pub(crate) fn refused(&mut self, what: &str, method: &str, path: &str, body: Option<Value>, want: u16) -> Option<Value> {
        let (status, v) = self.send(what, method, path, body)?;
        let shaped = v["error"]["type"].is_string() && v["error"]["message"].is_string();
        self.r.check(what, status == want && shaped, format!("returned {}: {}", status, v));
        Some(v)
    }

    pub(crate) fn decode<T: DeserializeOwned>(&mut self, what: &str, v: &Value) -> Option<T> {
        match serde_json::from_value::<T>(v.clone()) {
            Ok(t) => {
                self.r.ok_with(what, "");
                Some(t)
            }
            Err(e) => {
                self.r.fail(what, format!("{}: {}", e, v));
                None
            }
        }
    }

    pub(crate) fn ctx(&self, messages: Vec<Value>) -> Value {
        json!({"thinking": true, "tools": shell_tools(), "messages": messages})
    }

    /// The first messages of a context, unique to this run.
    pub(crate) fn opening(&self, label: &str) -> Vec<Value> {
        vec![
            json!({"role": "system", "content": format!("You are a coding agent ({} {}). Use the shell tool.", label, self.nonce)}),
            json!({"role": "user", "content": "Delete the build directory, then rebuild.", "snap": true}),
        ]
    }

    pub(crate) fn build(&mut self, what: &str, ctx: &Value, dry_run: bool) -> Option<Value> {
        let mut body = json!({"context": ctx});
        if dry_run {
            body["dry_run"] = json!(true);
        }
        self.expect(what, "POST", "/council/v2/contexts", Some(body), 200)
    }

    pub(crate) fn generate(&mut self, what: &str, ctx: &Value, max_tokens: u32) -> Option<Value> {
        let body = json!({"context": ctx, "max_tokens": max_tokens, "sample": {"temperature": 0}});
        self.expect(what, "POST", "/council/v2/generations", Some(body), 200)
    }

    /// The messages that continue a context after generation `g`: the reply with the call ids
    /// the server gave, then a tool message with the result of each call (or a user message when
    /// it made none), ending with a boundary.
    pub(crate) fn continue_after(&self, g: &Value, next_user: &str) -> Vec<Value> {
        let mut reply = json!({"role": "assistant"});
        for k in ["content", "reasoning"] {
            if g[k].as_str().is_some_and(|s| !s.is_empty()) {
                reply[k] = g[k].clone();
            }
        }
        let calls: Vec<Value> = g["tool_calls"]
            .as_array()
            .map(|a| a.iter().filter(|c| c["arguments"].is_object()).map(|c| json!({"id": c["id"], "name": c["name"], "arguments": c["arguments"]})).collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        if calls.is_empty() {
            out.push(reply);
            out.push(json!({"role": "user", "content": next_user, "snap": true}));
        } else {
            reply["tool_calls"] = json!(calls);
            out.push(reply);
            for (i, c) in calls.iter().enumerate() {
                let mut t = json!({"role": "tool", "tool_call_id": c["id"], "content": "Cargo.toml\nbuild/\nsrc/\n"});
                if i + 1 == calls.len() {
                    t["snap"] = json!(true);
                }
                out.push(t);
            }
        }
        out
    }

    // ---- identity

    pub(crate) fn identity(&mut self) -> bool {
        let Some(v) = self.expect("identity succeeds", "GET", "/council/v2/identity", None, 200) else { return false };
        let Some(id) = self.decode::<ServerIdentity>("identity has the contract's fields", &v) else { return false };
        let l = &id.limits;
        self.r.check("identity limits are positive", l.context_tokens > 0 && l.state_bytes > 0 && l.contexts_per_decision >= 1 && l.contexts_per_decision <= 8 && l.choice_options >= 2 && l.default_timeout_ms > 0, &id.limits);
        self.r.check("identity limits name the decision limit, pins, and snapshot cost", l.decision_context_tokens.is_some() && l.pin_bytes.is_some() && l.snapshot_bytes.is_some(), &id.limits);
        self.r.check(
            "identity decision_context_tokens is at most context_tokens",
            l.decision_context_tokens.is_none_or(|d| d <= l.context_tokens),
            &id.limits,
        );
        let unknown = id.capabilities.iter().filter(|c| **c == Capability::Unknown).count();
        self.r.ok_with("identity capabilities", format!("{} capabilities, {} unknown to gogi", id.capabilities.len(), unknown));
        self.identity = Some(id);
        true
    }

    // ---- messages and contexts the contract refuses

    pub(crate) fn validation(&mut self) {
        let user = json!({"role": "user", "content": "hi"});
        let call = |id: &str| json!({"id": id, "name": "shell", "arguments": {}});
        let asst = |id: &str| json!({"role": "assistant", "tool_calls": [call(id)]});
        let bad: Vec<(&str, Value)> = vec![
            ("a context with no messages", json!({"messages": []})),
            ("reasoning on a user message", json!({"messages": [{"role": "user", "content": "x", "reasoning": "r"}]})),
            ("tool_calls on a user message", json!({"messages": [{"role": "user", "content": "x", "tool_calls": [call("a")]}]})),
            ("a tool message without tool_call_id", json!({"messages": [user.clone(), asst("a"), {"role": "tool", "content": "r"}]})),
            ("a tool message naming no call", json!({"messages": [user.clone(), asst("a"), {"role": "tool", "tool_call_id": "nope", "content": "r"}]})),
            ("a tool message after a user message", json!({"messages": [asst("a"), user.clone(), {"role": "tool", "tool_call_id": "a", "content": "r"}]})),
            ("tool_call_id on a user message", json!({"messages": [{"role": "user", "content": "x", "tool_call_id": "a"}]})),
            ("a call with an empty id", json!({"messages": [user.clone(), asst("")]})),
            ("a call with no name", json!({"messages": [user.clone(), {"role": "assistant", "tool_calls": [{"id": "a", "name": "", "arguments": {}}]}]})),
            ("a call whose arguments are not an object", json!({"messages": [user.clone(), {"role": "assistant", "tool_calls": [{"id": "a", "name": "shell", "arguments": "x"}]}]})),
            ("a queue that names no tool", json!({"queue": "nope", "tools": shell_tools(), "messages": [user.clone()]})),
            ("an unknown role", json!({"messages": [{"role": "wizard", "content": "x"}]})),
            ("an unknown member of a context", json!({"messages": [user.clone()], "system": "x"})),
            ("an unknown member of a message", json!({"messages": [{"role": "user", "content": "x", "extra": 1}]})),
            ("a v1 context body", json!({"system": "s", "turns": [{"role": "user", "content": "x"}]})),
        ];
        for (name, ctx) in bad {
            self.refused(&format!("a build refuses {}", name), "POST", "/council/v2/contexts", Some(json!({"context": ctx})), 400);
        }
        self.refused("a build refuses an unknown member of the body", "POST", "/council/v2/contexts", Some(json!({"context": {"messages": [user.clone()]}, "persist": true})), 400);
        self.refused("a build refuses a body that is not JSON", "POST", "/council/v2/contexts", Some(json!("x")), 400);
        let later = self.ctx(vec![json!({"role": "system", "content": format!("s {}", self.nonce)}), user.clone(), json!({"role": "assistant", "content": "ok"}), json!({"role": "system", "content": "You replied without a tool call."})]);
        if let Some(b) = self.build("a system message may come after the first", &later, false) {
            self.r.check("a context that ends in a later system message has its head there", b["snapshots"].as_array().and_then(|s| s.last()).is_some_and(|s| s["message"] == 3), &b);
        }
        if let Some((status, v)) = self.send("the v1 context route is gone", "PUT", "/council/v2/contexts/old", Some(json!({"messages": [user]}))) {
            self.r.check("the v1 context route is gone", status == 404 || status == 405, format!("returned {}: {}", status, v));
        }
    }

    // ---- builds

    pub(crate) fn builds(&mut self) {
        let msgs = self.opening("build");
        let ctx = self.ctx(msgs.clone());
        let Some(b) = self.build("build succeeds", &ctx, false) else { return };
        let Some(built) = self.decode::<ContextBuilt>("build response has the contract's fields", &b) else { return };
        self.r.check("build: kept and fed make tokens", built.kept + built.fed == built.tokens && built.tokens > 0, &b);
        self.r.check("build: the last snapshot is the head", b["snapshots"].as_array().and_then(|s| s.last()).is_some_and(|s| s["id"] == b["head"] && s["tokens"] == b["tokens"]), &b);
        self.r.check("build: a marked user message has a snapshot", b["snapshots"].as_array().is_some_and(|s| s.iter().any(|x| x["message"] == 1)), &b);
        self.r.check("build: dry_run is false", b["dry_run"] == json!(false), &b);
        let ids: Vec<&Value> = b["snapshots"].as_array().map(|s| s.iter().map(|x| &x["id"]).collect()).unwrap_or_default();
        let mut tokens: Vec<i64> = b["snapshots"].as_array().map(|s| s.iter().map(|x| num(&x["tokens"])).collect()).unwrap_or_default();
        let before = tokens.clone();
        tokens.sort();
        tokens.dedup();
        self.r.check("build: snapshots run in order with distinct sizes", tokens == before && ids.len() == before.len(), &b["snapshots"]);

        if let Some(b2) = self.build("same context again succeeds", &ctx, false) {
            self.r.check("same context again feeds nothing", num(&b2["fed"]) == 0 && b2["head"] == b["head"] && num(&b2["kept"]) == num(&b["tokens"]), &b2);
        }
        if let Some(d) = self.build("dry run succeeds", &ctx, true) {
            self.r.check("a dry run of a built context feeds nothing", d["dry_run"] == json!(true) && num(&d["fed"]) == 0 && d["head"] == b["head"], &d);
        }

        // A longer context starts from the snapshot held at the shorter context's boundary.
        let mut longer = msgs.clone();
        longer.push(json!({"role": "assistant", "content": "I will look first."}));
        longer.push(json!({"role": "user", "content": "Go ahead.", "snap": true}));
        let lctx = self.ctx(longer);
        if let Some(l) = self.build("longer context succeeds", &lctx, false) {
            self.r.check("a longer context starts from the held boundary", num(&l["kept"]) > 0 && num(&l["fed"]) < num(&l["tokens"]), &l);
            self.r.check("a longer context keeps the shared boundary's id", l["snapshots"][0]["id"] == b["snapshots"][0]["id"], (&l["snapshots"], &b["snapshots"]));
        }

        // An edited earlier message shares only what precedes it.
        let edited = self.ctx(vec![msgs[0].clone(), json!({"role": "user", "content": "Only delete the build directory.", "snap": true})]);
        if let Some(e) = self.build("dry run of an edit succeeds", &edited, true) {
            self.r.check("dry run of an edit reports a fresh build", e["dry_run"] == json!(true) && num(&e["fed"]) > 0 && e["snapshots"] == json!([]), &e);
        }

        // Thinking off is a different context.
        let mut off = ctx.clone();
        off["thinking"] = json!(false);
        if let Some(o) = self.build("a context with thinking off succeeds", &off, false) {
            self.r.check("thinking is part of the context", o["head"] != b["head"], &o);
        }
    }

    /// A boundary exists only where the rendering keeps it.
    pub(crate) fn boundaries(&mut self) {
        let opening = self.opening("boundary");
        let mut answer = opening.clone();
        answer.push(json!({"role": "assistant", "reasoning": "The user wants a short note.", "content": "Noted.", "snap": true}));
        let c1 = self.ctx(answer.clone());
        let Some(b1) = self.build("an assistant message with reasoning and no tool calls builds", &c1, false) else { return };
        let snap_of = |b: &Value, message: i64| b["snapshots"].as_array().and_then(|s| s.iter().find(|x| num(&x["message"]) == message).cloned());
        self.r.check("the end of the context has a snapshot", snap_of(&b1, 2).is_some_and(|s| s["id"] == b1["head"]), &b1);
        answer.push(json!({"role": "user", "content": "Thanks. Now list the files.", "snap": true}));
        let c2 = self.ctx(answer);
        let Some(b2) = self.build("the same message followed by a user message builds", &c2, false) else { return };
        self.r.check("the end of a user message is a boundary", snap_of(&b2, 1).is_some_and(|s| Some(&s["id"]) == snap_of(&b1, 1).as_ref().map(|x| &x["id"])) && snap_of(&b2, 3).is_some_and(|s| s["id"] == b2["head"]), &b2["snapshots"]);
        match snap_of(&b2, 2) {
            Some(s) => {
                self.r.check("a boundary the rendering keeps is the held one", s["id"] == b1["head"], (&s, &b1["head"]));
                self.r.ok_with("a boundary the rendering drops gets no snapshot", "this template keeps earlier thinking, so there was no boundary to drop");
            }
            None => {
                self.r.check("a boundary the rendering drops gets no snapshot and is not reused", b2["from"] != b1["head"] && num(&b2["kept"]) < num(&b1["tokens"]), &b2);
            }
        }

        // The end of a run of tool results is always a boundary.
        let mut agent = opening;
        agent.push(json!({"role": "assistant", "reasoning": "I should list files.", "tool_calls": [{"id": "t0", "name": "shell", "arguments": {"command": "ls"}}], "snap": true}));
        agent.push(json!({"role": "tool", "tool_call_id": "t0", "content": "Cargo.toml\nsrc/\n", "snap": true}));
        let a1 = self.ctx(agent.clone());
        let Some(r1) = self.build("an agent turn builds", &a1, false) else { return };
        agent.push(json!({"role": "user", "content": "And the sizes?", "snap": true}));
        let a2 = self.ctx(agent);
        let Some(r2) = self.build("an agent turn followed by a user message builds", &a2, false) else { return };
        self.r.check("the end of a run of tool results stays a boundary after a user message", snap_of(&r2, 3).is_some() && snap_of(&r2, 4).is_some_and(|s| s["id"] == r2["head"]), &r2["snapshots"]);
        let both = snap_of(&r1, 2).is_some() && snap_of(&r2, 2).is_some();
        if both {
            self.r.check("a message with tool calls keeps its boundary and id", snap_of(&r1, 2).map(|s| s["id"].clone()) == snap_of(&r2, 2).map(|s| s["id"].clone()), (&r1["snapshots"], &r2["snapshots"]));
        }
    }
}
