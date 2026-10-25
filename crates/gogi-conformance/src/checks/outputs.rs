use gogi_proto::generation::{GenerationDone, StreamEvent};
use gogi_proto::identity::Capability;
use gogi_proto::{content_id, content_id_problem};
use serde_json::json;

use crate::{Suite, num};
use crate::http::sse_events;

/// A media type the suite assumes no served model writes.
const UNWRITTEN: &str = "application/x-gogi-conformance-unwritten";

impl Suite {
    /// Checks a generation that writes an output: the `output` event's pending id, the sealed
    /// output in `done`, its bytes and their id, the `308` of the pending id once sealed, the
    /// reply's message with its output, and `usage.media`.
    pub(crate) fn outputs(&mut self) {
        if !self.has(Capability::Generate) {
            self.r.skip("outputs", "the served model does not declare generate");
            return;
        }
        let outputs = self.model.as_ref().map(|m| m.outputs()).unwrap_or_default();
        let ctx = self.ctx(self.opening("outputs"));
        let Some(mime) = outputs.first().map(|p| match p.strip_suffix("/*") {
            Some(kind) => format!("{}/x-gogi-conformance", kind),
            None => p.clone(),
        }) else {
            let body = json!({"context": ctx, "max_tokens": 4, "output": {"mime": "image/png"}});
            self.refused_naming("an output to a model that writes text only is a 400 naming output.mime", "POST", "/council/v2/generations", Some(body), "output.mime", "image/png");
            self.r.skip("outputs", "the served model declares no media outputs");
            return;
        };
        if !self.has(Capability::Objects) {
            self.r.fail("a served model that writes media is on a server with objects", format!("it writes {:?}, but the server does not declare the objects capability", outputs));
            return;
        }
        let body = json!({"context": ctx, "max_tokens": 256, "sample": {"temperature": 0, "seed": 5}, "output": {"mime": mime}, "stream": true});
        let sent = self.routed("POST", "/council/v2/generations", Some(body.clone())).expect("a body stays a body");
        let r = match self.http.raw("POST", "/council/v2/generations", Some(&sent)) {
            Ok(r) => r,
            Err(e) => {
                self.r.fail("a generation with an output streams", e);
                return;
            }
        };
        self.note("POST", "/council/v2/generations", &r);
        let mut events = Vec::new();
        let mut faults = Vec::new();
        for (name, data) in sse_events(&r.text) {
            match StreamEvent::parse(&name, &data) {
                Ok(Some(e)) => events.push(e),
                Ok(None) => {}
                Err(e) => faults.push(e),
            }
        }
        self.r.check("a stream with an output decodes as the contract's events", faults.is_empty(), &faults);
        let started = events.iter().find_map(|e| if let StreamEvent::Output(o) = e { Some(o.clone()) } else { None });
        let done = events.iter().find_map(|e| if let StreamEvent::Done(d) = e { Some((**d).clone()) } else { None });
        let (Some(started), Some(done)) = (started, done) else {
            self.r.fail("a generation with an output sends an output event and done", format!("returned {}: {}", r.status, r.text.chars().take(400).collect::<String>()));
            return;
        };
        self.r.ok_with("a generation with an output sends an output event and done", "");
        self.r.check("an output event names a pending id", started.index == 0 && started.mime == mime && gogi_proto::ids::pending_id_problem(&started.pending).is_none(), &started);
        let Some(sealed) = done.outputs.first().cloned() else {
            self.r.fail("done names the sealed output", format!("{:?}", done.outputs));
            return;
        };
        self.r.check("done names the sealed output by content id", sealed.index == 0 && sealed.mime == mime && content_id_problem(sealed.id.as_str()).is_none(), &sealed);
        let path = format!("/council/v2/objects/{}", sealed.id);
        if let Some(g) = self.send_bytes("a sealed output reads back", "GET", &path, None, &[]) {
            self.r.check("a sealed output's bytes hash to its id", g.status == 200 && content_id(&g.body) == sealed.id.as_str() && g.body.len() as u64 == sealed.bytes, (g.status, g.body.len(), sealed.bytes));
            self.r.check("a sealed output reads back with its media type", g.header("content-type") == Some(mime.as_str()), g.header("content-type"));
        }
        let pending_path = format!("/council/v2/objects/{}", started.pending);
        for method in ["GET", "HEAD"] {
            let what = format!("{} of a sealed pending id is a 308 to its content id", method);
            match self.http.bytes_unfollowed(method, &pending_path) {
                Ok(p) => {
                    let to = p.header("location").unwrap_or("");
                    self.r.check(&what, p.status == 308 && (to == path || to.ends_with(&path)), (p.status, to));
                }
                Err(e) => self.r.fail(&what, e),
            }
        }
        let media = done.usage.media.unwrap_or_default();
        self.r.check("usage.media counts the output written", media.written.objects == 1 && media.written.bytes == sealed.bytes, done.usage);

        // The reply's message holds its output after its text, and the next build starts from the head.
        if let (Some(reply), Some(head)) = (done.as_message(), done.head.clone()) {
            let mut messages = self.opening("outputs");
            messages.push(serde_json::to_value(&reply).unwrap_or_default());
            let has_object = reply.content.as_ref().is_some_and(|c| c.parts().iter().any(|p| matches!(p, gogi_proto::Part::Object { id, .. } if *id == sealed.id.as_str())));
            self.r.check("a reply's message holds its output as an object part", has_object, &reply);
            if done.finish == gogi_proto::generation::Finish::MaxTokens {
                // A cut reply's head is its open rendering, which holds no parts (see Continuations).
                self.r.skip("a reply sent back with its output feeds nothing", "the reply was cut by max_tokens, so its head is its open rendering");
            } else if !self.model.as_ref().is_some_and(|m| m.text_replies()) {
                self.r.skip("a reply sent back with its output feeds nothing", "the served model takes no build: it has no text replies");
            } else if let Some(b) = self.build("a reply sent back with its output builds", &self.ctx(messages), false) {
                self.r.check("a reply sent back with its output feeds nothing", b["from"] == json!(head.as_str()) && num(&b["usage"]["fed"]) == 0, (&b["from"], head.as_str(), &b["usage"]));
            }
        }

        let mut plain = body.clone();
        plain["stream"] = json!(false);
        if let Some(v) = self.expect("a generation with an output and no stream succeeds", "POST", "/council/v2/generations", Some(plain), 200)
            && let Some(d) = self.decode::<GenerationDone>("a generation with an output decodes", &v)
        {
            self.r.check("a generation with an output and no stream names the sealed output", d.outputs.len() == 1 && d.outputs[0].mime == mime && content_id_problem(d.outputs[0].id.as_str()).is_none(), &d.outputs);
        }
        let base = self.ctx(self.opening("outputs"));
        let with = |output: serde_json::Value| json!({"context": base, "max_tokens": 4, "output": output});
        if !outputs.iter().any(|p| gogi_proto::identity::mime_matches(p, UNWRITTEN)) {
            self.refused_naming("an output of a media type the model does not write is a 400 naming output.mime", "POST", "/council/v2/generations", Some(with(json!({"mime": UNWRITTEN}))), "output.mime", UNWRITTEN);
        }
        self.refused_naming("an output media type in capitals is a 400 naming output.mime", "POST", "/council/v2/generations", Some(with(json!({"mime": "Image/PNG"}))), "output.mime", "Image/PNG");
        self.refused("an output with an unknown member is a 400", "POST", "/council/v2/generations", Some(with(json!({"mime": mime, "size": 3}))), 400);
        let unknown = format!("gogi_conformance_{}", self.nonce);
        self.refused_naming("an unknown output option is a 400 naming it", "POST", "/council/v2/generations", Some(with(json!({"mime": mime, "options": {unknown.clone(): 1}}))), &format!("output.options.{}", unknown), &unknown);

        let pending = format!("pending:{}", "0".repeat(64));
        if let Some(g) = self.send_bytes("a pending id the server never gave is a 404", "GET", &format!("/council/v2/objects/{}", pending), None, &[]) {
            self.r.check("a pending id the server never gave is a 404", g.status == 404, g.status);
        }
        let short = "pending:0123";
        if let Some(g) = self.send_bytes("a short pending id is a 400 naming it", "GET", &format!("/council/v2/objects/{}", short), None, &[]) {
            let v = g.json();
            self.r.check("a short pending id is a 400 naming it", g.status == 400 && v["error"]["param"] == json!("id") && v["error"]["message"].as_str().unwrap_or("").contains(short), &v);
        }
        // A pending id is not a content id, so no request body takes one.
        let owner = format!("/council/v2/keeps/gogi-conformance-pending-{}", self.nonce);
        self.refused_naming("a keep on a pending id is a 400 naming it", "PUT", &owner, Some(json!({"objects": [started.pending], "ttl_s": 60})), "objects", &started.pending);
        let mut m = self.opening("outputs");
        m.push(json!({"role": "user", "content": [{"type": "object", "id": started.pending, "mime": mime}]}));
        if self.model.as_ref().is_some_and(|m| m.text_replies()) {
            self.refused_naming("an object part with a pending id is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": self.ctx(m)})), "messages[2].content[0]", &started.pending);
        } else {
            let body = json!({"context": self.ctx(m), "max_tokens": 4, "output": {"mime": mime}});
            self.refused_naming("an object part with a pending id is a 400 naming the part", "POST", "/council/v2/generations", Some(body), "messages[2].content[0]", &started.pending);
        }
    }

    /// Checks a served model with no text replies: a build, a decision, and a raw read naming it
    /// are a `400` naming `model`, and so is no route but a generation with an `output`.
    pub(crate) fn writes_no_text(&mut self) {
        let ctx = self.ctx(self.opening("no-text"));
        let spec = format!("blake3:{}", "0".repeat(64));
        for (what, path, body) in [
            ("a build naming a model with no text replies is a 400 naming model", "/council/v2/contexts", json!({"context": ctx})),
            ("a decision naming a model with no text replies is a 400 naming model", "/council/v2/decisions", json!({"spec_id": spec, "case": "x", "contexts": [ctx]})),
            ("a raw read naming a model with no text replies is a 400 naming model", "/council/v2/reads", json!({"contexts": [ctx], "suffixes": ["A"], "letters": ["A", "B"]})),
        ] {
            if let Some((status, v)) = self.send(what, "POST", path, Some(body)) {
                self.r.check(what, status == 400 && v["error"]["param"] == json!("model"), format!("returned {}: {}", status, v));
            }
        }
        let body = json!({"context": ctx, "max_tokens": 4});
        if let Some((status, v)) = self.send("a generation with no output to a model with no text replies is a 400 naming output", "POST", "/council/v2/generations", Some(body)) {
            self.r.check("a generation with no output to a model with no text replies is a 400 naming output", status == 400 && v["error"]["param"] == json!("output"), format!("returned {}: {}", status, v));
        }
    }
}
