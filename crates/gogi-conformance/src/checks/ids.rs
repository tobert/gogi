use gogi_proto::{content_id, content_id_problem};
use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    /// Checks that every id a build gives is a whole content id, and that an id of another scheme,
    /// length, or case is a `400` that names the field and the id, never a lookup of a prefix.
    pub(crate) fn ids(&mut self) {
        let c = self.ctx(self.opening("ids"));
        let Some(b) = self.build("build for ids", &c, false) else { return };
        let mut given: Vec<String> = vec![b["head"].as_str().unwrap_or("").to_string()];
        given.extend(b["from"].as_str().map(String::from));
        given.extend(b["snapshots"].as_array().into_iter().flatten().map(|s| s["id"].as_str().unwrap_or("").to_string()));
        let bad: Vec<(String, String)> = given.iter().filter_map(|id| content_id_problem(id).map(|p| (id.clone(), p))).collect();
        self.r.check("every snapshot id a build gives is blake3 and 64 lowercase hex digits", bad.is_empty(), &bad);
        let head = given[0].clone();
        // Bad ids from hex that no held id contains, so a message that quotes a held id does not pass.
        let hex = content_id(format!("ids-{}", self.nonce).as_bytes()).trim_start_matches("blake3:").to_string();
        let short = format!("blake3:{}", &hex[..12]);
        let long = format!("blake3:{}0", hex);
        let upper = format!("blake3:{}", hex.to_ascii_uppercase());
        let sha = format!("sha256:{}", hex);
        let snap = format!("snap:{}", hex);

        for (what, id) in [("a short snapshot id", &short), ("a snapshot id of 65 digits", &long), ("a snapshot id in capitals", &upper), ("a snapshot id of another scheme", &sha)] {
            self.refused_naming(&format!("{} is a 400 naming it", what), "GET", &format!("/council/v2/snapshots/{}", id), None, "id", id);
        }

        let Some(spec_id) = self.hold_gate("ids") else { return };
        self.r.check("a spec id is blake3 and 64 lowercase hex digits", content_id_problem(&spec_id).is_none(), &spec_id);
        self.refused_naming("a short spec id is a 400 naming it", "GET", &format!("/council/v2/specs/{}", short), None, "id", &short);
        self.refused_naming("deleting a short spec id is a 400 naming it", "DELETE", &format!("/council/v2/specs/{}", short), None, "id", &short);
        for (what, id) in [("a short spec_id", &short), ("a sha256 spec_id", &sha)] {
            self.refused_naming(&format!("a decision with {} is a 400 naming it", what), "POST", "/council/v2/decisions", Some(json!({"spec_id": id, "case": "x", "contexts": [c.clone()]})), "spec_id", id);
        }
        if self.has(Capability::Ride) {
            for (what, id) in [("a short spec_id", &short), ("a sha256 spec_id", &sha)] {
                let body = json!({"context": c, "max_tokens": 4, "decisions": [{"spec_id": id, "when": "after"}]});
                self.refused_naming(&format!("a riding decision with {} is a 400 naming it", what), "POST", "/council/v2/generations", Some(body), "decisions", id);
            }
        } else {
            self.r.skip("a riding decision with a short spec_id is a 400 naming it", "the served model does not declare ride");
        }
        if self.has(Capability::Pins) {
            let path = format!("/council/v2/pins/gogi-conformance-ids-{}", self.nonce);
            for (what, id) in [("a short snapshot id", &short), ("a snap snapshot id", &snap), ("a sha256 snapshot id", &sha)] {
                self.refused_naming(&format!("a pin on {} is a 400 naming it", what), "PUT", &path, Some(json!({"snapshots": [head, id], "ttl_s": 60})), "snapshots", id);
            }
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
