use gogi_proto::content_id_problem;
use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    /// Checks that every id the server gives is a whole content id, and that a short or foreign id
    /// is a `400` that names the field and the id, never a lookup of a prefix.
    pub(crate) fn ids(&mut self) {
        let c = self.ctx(self.opening("ids"));
        let Some(b) = self.build("build for ids", &c, false) else { return };
        let mut given: Vec<String> = vec![b["head"].as_str().unwrap_or("").to_string()];
        given.extend(b["snapshots"].as_array().into_iter().flatten().map(|s| s["id"].as_str().unwrap_or("").to_string()));
        let bad: Vec<(String, String)> = given.iter().filter_map(|id| content_id_problem(id).map(|p| (id.clone(), p))).collect();
        self.r.check("every snapshot id is blake3 and 64 lowercase hex digits", bad.is_empty(), &bad);
        let head = given[0].clone();
        let short = head.chars().take("blake3:".len() + 12).collect::<String>();
        let foreign = format!("sha256:{}", head.strip_prefix("blake3:").unwrap_or(""));

        self.refused_naming("a short snapshot id is a 400 naming it", "GET", &format!("/council/v2/snapshots/{}", short), None, "id", &short);
        self.refused_naming("a snapshot id of another scheme is a 400 naming it", "GET", &format!("/council/v2/snapshots/{}", foreign), None, "id", &foreign);

        let Some(spec_id) = self.hold_gate("ids") else { return };
        self.r.check("a spec id is blake3 and 64 lowercase hex digits", content_id_problem(&spec_id).is_none(), &spec_id);
        let short_spec = spec_id.chars().take("blake3:".len() + 12).collect::<String>();
        self.refused_naming("a short spec id is a 400 naming it", "GET", &format!("/council/v2/specs/{}", short_spec), None, "id", &short_spec);
        self.refused_naming("deleting a short spec id is a 400 naming it", "DELETE", &format!("/council/v2/specs/{}", short_spec), None, "id", &short_spec);
        self.refused_naming("a decision with a short spec_id is a 400 naming it", "POST", "/council/v2/decisions", Some(json!({"spec_id": short_spec, "case": "x", "contexts": [c.clone()]})), "spec_id", &short_spec);
        if self.has(Capability::Ride) {
            let body = json!({"context": c, "max_tokens": 4, "decisions": [{"spec_id": short_spec, "when": "after"}]});
            self.refused_naming("a riding decision with a short spec_id is a 400 naming it", "POST", "/council/v2/generations", Some(body), "decisions", &short_spec);
        } else {
            self.r.skip("a riding decision with a short spec_id is a 400 naming it", "the served model does not declare ride");
        }
        if self.has(Capability::Pins) {
            let path = format!("/council/v2/pins/gogi-conformance-ids-{}", self.nonce);
            self.refused_naming("a pin on a short snapshot id is a 400 naming it", "PUT", &path, Some(json!({"snapshots": [head, short], "ttl_s": 60})), "snapshots", &short);
            self.refused("a refused pin set pins nothing", "GET", &path, None, 404);
        } else {
            self.r.skip("a pin on a short snapshot id is a 400 naming it", "the server does not declare the pins capability");
        }
        self.expect("ids spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }

    /// Checks that a request is a `400` whose `error.param` is `param` and whose message quotes
    /// `named`, so a reader sees which value was wrong.
    pub(crate) fn refused_naming(&mut self, what: &str, method: &str, path: &str, body: Option<Value>, param: &str, named: &str) {
        let Some((status, v)) = self.send(what, method, path, body) else { return };
        let message = v["error"]["message"].as_str().unwrap_or("");
        let ok = status == 400 && v["error"]["type"] == json!("invalid_request") && v["error"]["param"] == json!(param) && message.contains(named);
        self.r.check(what, ok, format!("returned {}: {}", status, v));
    }
}
