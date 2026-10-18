//! The checks, grouped by what they test.

mod decisions;
mod generations;
mod headers;
mod models;
mod pins;
mod reads;
mod restart;
mod ride;

use gogi_proto::context::ContextBuilt;
use gogi_proto::identity::{Capability, ModelLimits, ModelStatus, ServerIdentity};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::http::Response;
use crate::{Suite, num};

pub(crate) fn shell_tools() -> Value {
    json!([{"name": "shell", "description": "Runs a shell command.",
        "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}}}])
}

impl Suite {
    /// Whether the server or the checked served model declares a capability.
    pub(crate) fn has(&self, c: Capability) -> bool {
        self.identity.as_ref().is_some_and(|i| i.has(c)) || self.model.as_ref().is_some_and(|m| m.has(c))
    }

    /// The checked served model's limits.
    pub(crate) fn limits(&self) -> Option<&ModelLimits> {
        self.model.as_ref().map(|m| &m.limits)
    }

    /// The checked served model's `model`.
    pub(crate) fn model_name(&self) -> String {
        self.model.as_ref().map_or_else(String::new, |m| m.model.clone())
    }

    /// Names the checked served model in a body for a route that reads a context, unless the body
    /// names a model already. Other bodies pass unchanged.
    pub(crate) fn routed(&self, method: &str, path: &str, body: Option<Value>) -> Option<Value> {
        let reads = ["/council/v2/contexts", "/council/v2/decisions", "/council/v2/reads", "/council/v2/generations"];
        match body {
            Some(Value::Object(mut b)) if method == "POST" && reads.contains(&path) => {
                b.entry("model").or_insert_with(|| json!(self.model_name()));
                Some(Value::Object(b))
            }
            other => other,
        }
    }

    /// Sends a request and returns its status and body. A transport failure is recorded. A body for
    /// a route that reads a context names the checked served model unless it names one.
    pub(crate) fn send(&mut self, what: &str, method: &str, path: &str, body: Option<Value>) -> Option<(u16, Value)> {
        let body = self.routed(method, path, body);
        self.send_as_written(what, method, path, body.as_ref())
    }

    /// Sends a request with its body as written. A transport failure is recorded.
    pub(crate) fn send_as_written(&mut self, what: &str, method: &str, path: &str, body: Option<&Value>) -> Option<(u16, Value)> {
        match self.http.raw(method, path, body) {
            Ok(r) => {
                self.note(method, path, &r);
                Some((r.status, r.json()))
            }
            Err(e) => {
                self.r.fail(what, e);
                None
            }
        }
    }

    /// Records a response's `request-id`, and a fault when the header is missing or malformed or
    /// an error body's `request_id` differs from it.
    pub(crate) fn note(&mut self, method: &str, path: &str, r: &Response) {
        let Some(id) = r.request_id.clone().filter(|i| gogi_proto::ids::is_request_id(i)) else {
            self.request_id_faults.push(format!("{} {} returned {} with request-id {:?}", method, path, r.status, r.request_id));
            return;
        };
        let v = r.json();
        if r.status >= 400 && v.get("error").is_some() && v["request_id"] != json!(id) {
            self.request_id_faults.push(format!("{} {} returned {} with request-id {} and an error body whose request_id is {}", method, path, r.status, id, v["request_id"]));
        }
        self.request_ids.push(id);
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
            .map(|a| a.iter().filter(|c| c["input"].is_object()).map(|c| json!({"id": c["id"], "name": c["name"], "input": c["input"]})).collect())
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

    /// Reads identity and chooses the served model to check: `want`, by name or alias, or else the
    /// first `ready` one.
    pub(crate) fn identity(&mut self, want: Option<&str>) -> bool {
        let Some(v) = self.expect("identity succeeds", "GET", "/council/v2/identity", None, 200) else { return false };
        let Some(id) = self.decode::<ServerIdentity>("identity has the contract's fields", &v) else { return false };
        let l = &id.limits;
        self.r.check("identity server limits are positive", l.case_bytes > 0 && l.choice_options >= 2, l);
        self.r.check("identity server limits name pin_bytes when the server has pins", l.pin_bytes.is_some() || !id.has(Capability::Pins), l);
        self.r.check("identity lists at least one served model", !id.models.is_empty(), &v["models"]);
        let mut names: Vec<&str> = id.models.iter().flat_map(|m| std::iter::once(m.model.as_str()).chain(m.aliases.iter().map(String::as_str))).collect();
        let all = names.len();
        names.sort();
        names.dedup();
        self.r.check("each served model's name and aliases name no other", names.len() == all, &names);
        for m in &id.models {
            let l = &m.limits;
            self.r.check(
                &format!("{}: limits are positive", m.model),
                l.context_tokens > 0 && l.decision_context_tokens > 0 && l.snapshot_bytes > 0 && (1..=8).contains(&l.contexts_per_decision) && l.default_timeout_ms > 0,
                l,
            );
            self.r.check(&format!("{}: decision_context_tokens is at most context_tokens", m.model), l.decision_context_tokens <= l.context_tokens, l);
            self.r.check(&format!("{}: its status is one the contract names", m.model), m.status != ModelStatus::Unknown, &m.status);
        }
        let caps: Vec<&Capability> = id.capabilities.iter().chain(id.models.iter().flat_map(|m| &m.capabilities)).collect();
        let unknown = caps.iter().filter(|c| ***c == Capability::Unknown).count();
        self.r.ok_with("identity capabilities", format!("{} capabilities, {} unknown to gogi", caps.len(), unknown));
        let chosen = match want {
            Some(name) => id.route(name).ok().filter(|m| m.status == ModelStatus::Ready),
            None => id.models.iter().find(|m| m.status == ModelStatus::Ready),
        };
        let Some(m) = chosen.cloned() else {
            let statuses: Vec<String> = id.models.iter().map(|m| format!("{} {:?}", m.model, m.status)).collect();
            self.r.fail("a served model to check is ready", format!("no checks ran: {} is not a ready served model; identity lists {:?}", want.unwrap_or("any model"), statuses));
            return false;
        };
        self.r.ok_with("a served model to check is ready", m.model.clone());
        self.model = Some(m);
        self.identity = Some(id);
        true
    }

    // ---- messages and contexts the contract refuses

    pub(crate) fn validation(&mut self) {
        let user = json!({"role": "user", "content": "hi"});
        let call = |id: &str| json!({"id": id, "name": "shell", "input": {}});
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
            ("a call with no name", json!({"messages": [user.clone(), {"role": "assistant", "tool_calls": [{"id": "a", "name": "", "input": {}}]}]})),
            ("a call whose input is not an object", json!({"messages": [user.clone(), {"role": "assistant", "tool_calls": [{"id": "a", "name": "shell", "input": "x"}]}]})),
            ("a call with arguments in place of input", json!({"messages": [user.clone(), {"role": "assistant", "tool_calls": [{"id": "a", "name": "shell", "arguments": {}}]}]})),
            ("is_error on a user message", json!({"messages": [{"role": "user", "content": "x", "is_error": true}]})),
            ("a tool in OpenAI's shape", json!({"tools": [{"type": "function", "function": {"name": "shell", "parameters": {}}}], "messages": [user.clone()]})),
            ("a tool without input_schema", json!({"tools": [{"name": "shell"}], "messages": [user.clone()]})),
            ("two tools with one name", json!({"tools": [shell_tools()[0].clone(), shell_tools()[0].clone()], "messages": [user.clone()]})),
            ("a queue that names no tool", json!({"queue": "nope", "tools": shell_tools(), "messages": [user.clone()]})),
            ("an unknown role", json!({"messages": [{"role": "wizard", "content": "x"}]})),
            ("an unknown member of a context", json!({"messages": [user.clone()], "system": "x"})),
            ("an unknown member of a message", json!({"messages": [{"role": "user", "content": "x", "extra": 1}]})),
        ];
        for (name, ctx) in bad {
            self.refused(&format!("a build refuses {}", name), "POST", "/council/v2/contexts", Some(json!({"context": ctx})), 400);
        }
        self.refused("a build refuses an unknown member of the body", "POST", "/council/v2/contexts", Some(json!({"context": {"messages": [user.clone()]}, "persist": true})), 400);
        self.refused("a build refuses a body that is not JSON", "POST", "/council/v2/contexts", Some(json!("x")), 400);
        let mut failed = vec![user.clone(), asst("a"), json!({"role": "tool", "tool_call_id": "a", "content": format!("exit 1 {}", self.nonce), "is_error": true})];
        if let Some(f) = self.build("a failed tool result builds", &self.ctx(failed.clone()), false) {
            failed[2]["is_error"] = json!(false);
            if let Some(ok) = self.build("the same tool result, not failed, builds", &self.ctx(failed), false) {
                self.r.check("is_error is part of the context", f["head"] != ok["head"], (&f["head"], &ok["head"]));
            }
        }
        let later = self.ctx(vec![json!({"role": "system", "content": format!("s {}", self.nonce)}), user.clone(), json!({"role": "assistant", "content": "ok"}), json!({"role": "system", "content": "You replied without a tool call."})]);
        if let Some(b) = self.build("a system message may come after the first", &later, false) {
            self.r.check("a context that ends in a later system message has its head there", b["snapshots"].as_array().and_then(|s| s.last()).is_some_and(|s| s["message"] == 3), &b);
        }
    }

    // ---- builds

    pub(crate) fn builds(&mut self) {
        let msgs = self.opening("build");
        let ctx = self.ctx(msgs.clone());
        let Some(b) = self.build("build succeeds", &ctx, false) else { return };
        let Some(built) = self.decode::<ContextBuilt>("build response has the contract's fields", &b) else { return };
        let u = built.usage;
        self.r.check("build usage: kept and fed make prompt", u.check().is_ok() && u.prompt > 0, &b["usage"]);
        self.r.check("build usage: a build writes nothing", u.completion == 0 && u.reasoning == 0, &b["usage"]);
        self.r.check("build: the last snapshot is the head", b["snapshots"].as_array().and_then(|s| s.last()).is_some_and(|s| s["id"] == b["head"] && s["tokens"] == b["usage"]["prompt"]), &b);
        self.r.check("build: a marked user message has a snapshot", b["snapshots"].as_array().is_some_and(|s| s.iter().any(|x| x["message"] == 1)), &b);
        self.r.check("build: dry_run is false", b["dry_run"] == json!(false), &b);
        let ids: Vec<&Value> = b["snapshots"].as_array().map(|s| s.iter().map(|x| &x["id"]).collect()).unwrap_or_default();
        let mut tokens: Vec<i64> = b["snapshots"].as_array().map(|s| s.iter().map(|x| num(&x["tokens"])).collect()).unwrap_or_default();
        let before = tokens.clone();
        tokens.sort();
        tokens.dedup();
        self.r.check("build: snapshots run in order with distinct sizes", tokens == before && ids.len() == before.len(), &b["snapshots"]);

        if let Some(b2) = self.build("same context again succeeds", &ctx, false) {
            self.r.check("same context again feeds nothing", num(&b2["usage"]["fed"]) == 0 && b2["head"] == b["head"] && num(&b2["usage"]["kept"]) == num(&b["usage"]["prompt"]), &b2);
        }
        if let Some(d) = self.build("dry run succeeds", &ctx, true) {
            self.r.check("a dry run of a built context feeds nothing", d["dry_run"] == json!(true) && num(&d["usage"]["fed"]) == 0 && d["head"] == b["head"], &d);
        }

        // A longer context starts from the snapshot held at the shorter context's boundary.
        let mut longer = msgs.clone();
        longer.push(json!({"role": "assistant", "content": "I will look first."}));
        longer.push(json!({"role": "user", "content": "Go ahead.", "snap": true}));
        let lctx = self.ctx(longer);
        if let Some(l) = self.build("longer context succeeds", &lctx, false) {
            self.r.check("a longer context starts from the held boundary", num(&l["usage"]["kept"]) > 0 && num(&l["usage"]["fed"]) < num(&l["usage"]["prompt"]), &l);
            self.r.check("a longer context keeps the shared boundary's id", l["snapshots"][0]["id"] == b["snapshots"][0]["id"], (&l["snapshots"], &b["snapshots"]));
        }

        // An edited earlier message shares only what precedes it.
        let edited = self.ctx(vec![msgs[0].clone(), json!({"role": "user", "content": "Only delete the build directory.", "snap": true})]);
        if let Some(e) = self.build("dry run of an edit succeeds", &edited, true) {
            self.r.check("dry run of an edit reports a fresh build", e["dry_run"] == json!(true) && num(&e["usage"]["fed"]) > 0 && e["snapshots"] == json!([]), &e);
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
                self.r.check("a boundary the rendering drops gets no snapshot and is not reused", b2["from"] != b1["head"] && num(&b2["usage"]["kept"]) < num(&b1["usage"]["prompt"]), &b2);
            }
        }

        // The end of a run of tool results is always a boundary.
        let mut agent = opening;
        agent.push(json!({"role": "assistant", "reasoning": "I should list files.", "tool_calls": [{"id": "t0", "name": "shell", "input": {"command": "ls"}}], "snap": true}));
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
