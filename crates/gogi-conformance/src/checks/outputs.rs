use gogi_proto::generation::{GenerationDone, StreamEvent};
use gogi_proto::identity::Capability;
use gogi_proto::{content_id, content_id_problem};
use serde_json::json;

use crate::Suite;
use crate::http::sse_events;

/// A media type the suite assumes no served model writes.
const UNWRITTEN: &str = "application/x-gogi-conformance-unwritten";

impl Suite {
    /// Checks a generation that writes an object: the `part` event's pending id, the sealed part in
    /// `done`, its bytes and their id, the pending id once the part is sealed, and `usage.media`.
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
        let body = json!({"context": ctx, "max_tokens": 4, "sample": {"temperature": 0, "seed": 5}, "output": {"mime": mime}, "stream": true});
        let Some(r) = self.http.raw("POST", "/council/v2/generations", Some(&self.routed("POST", "/council/v2/generations", Some(body.clone())).unwrap())).ok() else {
            self.r.fail("a generation with an output streams", "the request failed");
            return;
        };
        self.note("POST", "/council/v2/generations", &r);
        let events: Vec<StreamEvent> = sse_events(&r.text).iter().filter_map(|(n, d)| StreamEvent::parse(n, d).ok().flatten()).collect();
        let part = events.iter().find_map(|e| if let StreamEvent::Part(p) = e { Some(p.clone()) } else { None });
        let done = events.iter().find_map(|e| if let StreamEvent::Done(d) = e { Some((**d).clone()) } else { None });
        let (Some(part), Some(done)) = (part, done) else {
            self.r.fail("a generation with an output sends a part event and done", format!("returned {}: {}", r.status, r.text.chars().take(400).collect::<String>()));
            return;
        };
        self.r.ok_with("a generation with an output sends a part event and done", "");
        self.r.check("a part event names a pending id", part.index == 0 && part.mime == mime && gogi_proto::ids::pending_id_problem(&part.pending).is_none(), &part);
        let Some(sealed) = done.parts.first().cloned() else {
            self.r.fail("done names the sealed part", format!("{:?}", done.parts));
            return;
        };
        self.r.check("done names the sealed part by content id", sealed.index == 0 && sealed.mime == mime && content_id_problem(sealed.id.as_str()).is_none(), &sealed);
        let path = format!("/council/v2/objects/{}", sealed.id);
        if let Some(g) = self.send_bytes("a sealed part reads back", "GET", &path, None, &[]) {
            self.r.check("a sealed part's bytes hash to its id", g.status == 200 && content_id(&g.body) == sealed.id.as_str() && g.body.len() as u64 == sealed.bytes, (g.status, g.body.len(), sealed.bytes));
            self.r.check("a sealed part reads back with its media type", g.header("content-type") == Some(mime.as_str()), g.header("content-type"));
            if let Some(p) = self.send_bytes("a pending id reads the sealed part", "GET", &format!("/council/v2/objects/{}", part.pending), None, &[]) {
                self.r.check("a pending id reads the sealed part", p.status == 200 && p.body == g.body, (p.status, p.body.len()));
            }
        }
        let media = done.usage.media.unwrap_or_default();
        self.r.check("usage.media counts the object written", media.written.objects == 1 && media.written.bytes == sealed.bytes, &done.usage);

        let mut plain = body.clone();
        plain["stream"] = json!(false);
        if let Some(v) = self.expect("a generation with an output and no stream succeeds", "POST", "/council/v2/generations", Some(plain), 200)
            && let Some(d) = self.decode::<GenerationDone>("a generation with an output decodes", &v)
        {
            self.r.check("a generation with an output and no stream names the sealed part", d.parts.len() == 1 && d.parts[0].mime == mime && content_id_problem(d.parts[0].id.as_str()).is_none(), &d.parts);
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
    }
}
