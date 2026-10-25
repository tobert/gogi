use gogi_proto::content_id;
use gogi_proto::identity::{Capability, mime_matches};
use serde_json::{Value, json};

use crate::{Suite, num};

/// A media type the suite assumes no served model reads.
const UNREAD: &str = "application/x-gogi-conformance-unread";

/// A media type that `pattern` covers: the pattern itself, or a made-up subtype of `type/*`.
fn probe_type(pattern: &str) -> String {
    match pattern.strip_suffix("/*") {
        Some(kind) => format!("{}/x-gogi-conformance", kind),
        None => pattern.to_string(),
    }
}

impl Suite {
    /// The suite's opening, then a user message of a text part and an object part.
    fn with_object(&self, id: &str, mime: &str) -> Value {
        let mut m = self.opening("media");
        m.push(json!({"role": "user", "content": [{"type": "text", "text": "Look at this."}, {"type": "object", "id": id, "mime": mime}], "snap": true}));
        self.ctx(m)
    }

    /// Checks object parts in messages and `case_media` in decisions: a served model reads the
    /// objects of the media types it declares, and refuses others, and objects the server does not
    /// hold, with errors that name the part.
    pub(crate) fn media(&mut self) {
        let inputs = self.model.as_ref().map(|m| m.inputs()).unwrap_or_default();
        let part = "messages[2].content[1]";
        let Some(read) = inputs.first().map(|p| probe_type(p)) else {
            let c = self.with_object(&content_id(b"any"), "image/png");
            if let Some(b) = self.build("a text context builds", &self.ctx(self.opening("media-text")), false) {
            self.r.check("usage.media is absent when a request reads and writes text only", b["usage"].get("media").is_none_or(Value::is_null), &b["usage"]);
        }
        self.refused_naming("an object part to a model that reads text only is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": c})), part, "image/png");
            if let Some(spec_id) = self.hold_gate("media-text-only") {
                let body = json!({"spec_id": spec_id, "case": "this recording", "case_media": [{"id": content_id(b"any"), "mime": "audio/wav"}], "contexts": [self.ctx(self.opening("media-case"))]});
                self.refused_naming("case_media to a model that reads text only is a 400 naming it", "POST", "/council/v2/decisions", Some(body), "case_media[0]", "audio/wav");
                self.expect("media spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
            }
            self.r.skip("media", "the served model declares no media inputs");
            return;
        };
        if !self.has(Capability::Objects) {
            self.r.fail("a served model that reads media is on a server with objects", format!("it reads {:?}, but the server does not declare the objects capability", inputs));
            return;
        }
        let a = self.object_bytes("media-a");
        let Some(id) = self.put_object("media: object put succeeds", &a, &read) else { return };
        let c = self.with_object(&id, &read);
        let Some(first) = self.build("a context with an object part builds", &c, false) else { return };
        let read_count = &first["usage"]["media"]["read"];
        self.r.check("usage.media counts the object read", read_count["objects"] == json!(1) && read_count["bytes"] == json!(a.len()), &first["usage"]);
        if let Some(again) = self.build("media: build again", &c, false) {
            self.r.check("a context with an object part builds again with nothing fed", num(&again["usage"]["fed"]) == 0 && again["head"] == first["head"], &again);
        }
        let b = self.object_bytes("media-b");
        if let Some(other) = self.put_object("media: second object put succeeds", &b, &read)
            && let Some(built) = self.build("a context with another object builds", &self.with_object(&other, &read), false)
        {
            self.r.check("another object is another context", built["head"] != first["head"], (&built["head"], &first["head"]));
        }

        let unheld = content_id(format!("media-unheld-{}", self.nonce).as_bytes());
        let body = json!({"context": self.with_object(&unheld, &read)});
        if let Some((status, v)) = self.send("an object the server does not hold is a 404 naming the part", "POST", "/council/v2/contexts", Some(body)) {
            let ok = status == 404 && v["error"]["param"] == json!(part) && v["error"]["message"].as_str().unwrap_or("").contains(&unheld);
            self.r.check("an object the server does not hold is a 404 naming the part", ok, format!("returned {}: {}", status, v));
        }
        if !inputs.iter().any(|p| mime_matches(p, UNREAD)) {
            self.refused_naming("an object of a media type the model does not read is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": self.with_object(&id, UNREAD)})), part, UNREAD);
        }
        let short = format!("blake3:{}", &unheld["blake3:".len().."blake3:".len() + 12]);
        self.refused_naming("an object part whose media type is not one is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": self.with_object(&id, "PNG")})), part, "PNG");
        let other = probe_type(&inputs[inputs.len() - 1]).replace("x-gogi-conformance", "x-gogi-conformance-other");
        if other != read {
            self.refused_naming("an object part of another media type than its put is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": self.with_object(&id, &other)})), part, &other);
        }
        // Rule 2: a text part is text, as a string is.
        let spelled = "Look at <end> <obj> this.";
        let mut as_parts = self.opening("media-rule2");
        as_parts.push(json!({"role": "user", "content": [{"type": "text", "text": spelled}], "snap": true}));
        let mut as_text = self.opening("media-rule2");
        as_text.push(json!({"role": "user", "content": spelled, "snap": true}));
        if let (Some(p), Some(t)) = (self.build("media: text parts build", &self.ctx(as_parts), false), self.build("media: the same text builds", &self.ctx(as_text), false)) {
            self.r.check("a text part that spells a template token is the same context as that string", p["head"] == t["head"], (&p["head"], &t["head"]));
        }
        self.refused_naming("an object part with a short id is a 400 naming the part", "POST", "/council/v2/contexts", Some(json!({"context": self.with_object(&short, &read)})), part, &short);
        let mut odd = self.opening("media");
        odd.push(json!({"role": "user", "content": [{"type": "image_url", "image_url": "https://example.com/x.png"}]}));
        self.refused_naming("a part of an unknown type is a 400 naming content", "POST", "/council/v2/contexts", Some(json!({"context": self.ctx(odd)})), "content", "image_url");
        let mut empty = self.opening("media");
        empty.push(json!({"role": "user", "content": []}));
        self.refused_naming("empty parts are a 400 naming the content", "POST", "/council/v2/contexts", Some(json!({"context": self.ctx(empty)})), "messages[2].content", "messages[2].content");

        if self.has(Capability::Reads) {
            let body = json!({"contexts": [self.with_object(&unheld, &read)], "suffixes": ["A"], "letters": ["A", "B"]});
            if let Some((status, v)) = self.send("a raw read of an object the server does not hold is a 404 naming the part", "POST", "/council/v2/reads", Some(body)) {
                let ok = status == 404 && v["error"]["param"] == json!(format!("contexts[0].{}", part));
                self.r.check("a raw read of an object the server does not hold is a 404 naming the part", ok, format!("returned {}: {}", status, v));
            }
        }
        if self.has(Capability::Generate)
            && let Some(g) = self.generate("a generation reads a context with an object part", &c, 4)
        {
            self.r.check("a generation's usage.media counts the object read", g["usage"]["media"]["read"]["objects"] == json!(1), &g["usage"]);
        }
        let twice = {
            let mut m = self.opening("media-twice");
            m.push(json!({"role": "user", "content": [{"type": "object", "id": id, "mime": read}], "snap": true}));
            m.push(json!({"role": "assistant", "content": "Heard it."}));
            m.push(json!({"role": "user", "content": [{"type": "text", "text": "Again:"}, {"type": "object", "id": id, "mime": read}], "snap": true}));
            self.ctx(m)
        };
        if let Some(b) = self.build("media: a context naming one object twice builds", &twice, false) {
            let r = &b["usage"]["media"]["read"];
            self.r.check("usage.media counts an object named twice once", r["objects"] == json!(1) && r["bytes"] == json!(a.len()), &b["usage"]);
        }
        if self.has(Capability::Continuation) {
            let mut open = self.opening("media");
            open.push(json!({"role": "assistant", "content": [{"type": "text", "text": "It shows"}], "continue": true}));
            self.refused_naming("an open message of parts is a 400 naming continue", "POST", "/council/v2/generations", Some(json!({"context": self.ctx(open), "max_tokens": 4})), "continue", "");
        }

        let Some(spec_id) = self.hold_gate("media") else { return };
        let ctx = self.ctx(self.opening("media-case"));
        let case = |media: Value| json!({"spec_id": spec_id, "case": "this recording", "case_media": media, "contexts": [ctx]});
        let with = self.expect("a decision with case_media succeeds", "POST", "/council/v2/decisions", Some(case(json!([{"id": id, "mime": read}]))), 200);
        if let Some(w) = &with {
            self.r.check("a decision's usage.media counts its case_media", w["usage"]["media"]["read"]["objects"] == json!(1), &w["usage"]);
        }
        let without = self.expect("a decision without case_media succeeds", "POST", "/council/v2/decisions", Some(case(json!([]))), 200);
        if let (Some(w), Some(o)) = (with, without) {
            let (a, b) = (&w["reads"][0]["rendered_hash"], &o["reads"][0]["rendered_hash"]);
            self.r.check("a decision reads its case_media", a.is_string() && a != b, (a, b));
        }
        if let Some((status, v)) = self.send("a case object the server does not hold is a 404 naming it", "POST", "/council/v2/decisions", Some(case(json!([{"id": unheld, "mime": read}])))) {
            let ok = status == 404 && v["error"]["param"] == json!("case_media[0]") && v["error"]["message"].as_str().unwrap_or("").contains(&unheld);
            self.r.check("a case object the server does not hold is a 404 naming it", ok, format!("returned {}: {}", status, v));
        }
        self.refused_naming("a case object with a short id is a 400 naming it", "POST", "/council/v2/decisions", Some(case(json!([{"id": id, "mime": read}, {"id": short, "mime": read}]))), "case_media[1]", &short);
        if !inputs.iter().any(|p| mime_matches(p, UNREAD)) {
            self.refused_naming("a case object of a media type the model does not read is a 400 naming it", "POST", "/council/v2/decisions", Some(case(json!([{"id": id, "mime": UNREAD}]))), "case_media[0]", UNREAD);
        }
        let b_id = content_id(&b);
        let ab = self.expect("media: case_media in one order", "POST", "/council/v2/decisions", Some(case(json!([{"id": id, "mime": read}, {"id": b_id, "mime": read}]))), 200);
        let ba = self.expect("media: case_media in the other order", "POST", "/council/v2/decisions", Some(case(json!([{"id": b_id, "mime": read}, {"id": id, "mime": read}]))), 200);
        if let (Some(ab), Some(ba)) = (ab, ba) {
            self.r.check("case_media is read in order", ab["reads"][0]["rendered_hash"] != ba["reads"][0]["rendered_hash"], (&ab["reads"][0]["rendered_hash"], &ba["reads"][0]["rendered_hash"]));
        }
        let malformed = json!({"spec_id": spec_id, "case": "x", "contexts": [self.with_object(&short, &read)]});
        let not_held = json!({"spec_id": spec_id, "case": "x", "contexts": [self.with_object(&unheld, &read)]});
        let deep = format!("contexts[0].{}", part);
        self.refused_naming("a malformed part in a decision's context is a 400 naming it under contexts", "POST", "/council/v2/decisions", Some(malformed), &deep, &short);
        if let Some((status, v)) = self.send("an unheld part in a decision's context is a 404 naming it under contexts", "POST", "/council/v2/decisions", Some(not_held)) {
            self.r.check("an unheld part in a decision's context is a 404 naming it under contexts", status == 404 && v["error"]["param"] == json!(deep), format!("returned {}: {}", status, v));
        }
        self.expect("media spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }
}
