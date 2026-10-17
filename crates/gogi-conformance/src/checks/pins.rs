use gogi_proto::identity::Capability;
use gogi_proto::pins::{PIN_TTL_MAX_S, PinsPutResult, SnapshotState};
use serde_json::{Value, json};

use crate::{Suite, num};

impl Suite {
    pub(crate) fn pins(&mut self) {
        // Two contexts, the second built on the first, so the first is in the second's chain.
        let msgs = self.opening("pin");
        let c1 = self.ctx(msgs.clone());
        let Some(b1) = self.build("build for pins", &c1, false) else { return };
        let mut m2 = msgs.clone();
        let mut reply = json!({"role": "assistant", "content": "Looking."});
        reply["snap"] = json!(false);
        m2.push(reply);
        m2.push(json!({"role": "user", "content": "Go on.", "snap": true}));
        let c2 = self.ctx(m2);
        let Some(b2) = self.build("build on a held prefix for pins", &c2, false) else { return };
        let (first, head) = (b1["head"].as_str().unwrap_or("").to_string(), b2["head"].as_str().unwrap_or("").to_string());
        self.r.check("the second context is built on the first", b2["from"] == b1["head"] || num(&b2["kept"]) >= num(&b1["tokens"]), &b2);

        if let Some(s) = self.expect("snapshot info answers", "GET", &format!("/council/v2/snapshots/{}", head), None, 200) {
            if let Some(st) = self.decode::<SnapshotState>("snapshot info has the contract's fields", &s) {
                self.r.check("snapshot info: tokens match the build", st.tokens as i64 == num(&b2["tokens"]) && st.pins.is_empty(), &s);
            }
        }
        self.refused("an unknown snapshot is a 404", "GET", &format!("/council/v2/snapshots/snap:{}", "0".repeat(64)), None, 404);

        if !self.has(Capability::Pins) {
            self.r.skip("pins", "the server does not declare the pins capability");
            return;
        }
        let owner = format!("gogi-conformance-{}", self.nonce);
        let path = format!("/council/v2/pins/{}", owner);
        let unheld = format!("snap:{}", "0".repeat(64));
        self.refused("an owner with nothing is a 404", "GET", &path, None, 404);
        self.refused("deleting an owner with nothing is a 404", "DELETE", &path, None, 404);

        let Some(p) = self.expect("pin answers", "PUT", &path, Some(json!({"snapshots": [head, unheld], "ttl_s": 120})), 200) else { return };
        if let Some(r) = self.decode::<PinsPutResult>("pin reply has the contract's fields", &p) {
            self.r.check("a pin on an unknown snapshot reports held false", r.snapshots.len() == 2 && r.snapshots[0].held && !r.snapshots[1].held, &p);
            self.r.check("a pin's time is at most the ttl asked", r.expires_in_s.is_some_and(|e| 0 < e && e <= 120), &p);
        }
        if let Some(s) = self.expect("pinned snapshot info answers", "GET", &format!("/council/v2/snapshots/{}", head), None, 200) {
            self.r.check("a snapshot names the owner that pins it", s["pins"].as_array().is_some_and(|p| p.iter().any(|x| x["owner"] == json!(owner) && num(&x["expires_in_s"]) > 0)), &s);
        }
        if let Some(s) = self.expect("chained snapshot info answers", "GET", &format!("/council/v2/snapshots/{}", first), None, 200) {
            self.r.check("a chain is kept, not named", s["pins"].as_array().is_some_and(|p| p.iter().all(|x| x["owner"] != json!(owner))), &s);
        }
        if let Some(l) = self.expect("pins list answers", "GET", &path, None, 200) {
            let ids = pinned_ids(&l);
            self.r.check("pins list the held snapshots only", ids == [head.clone()] && l["owner"] == json!(owner) && num(&l["expires_in_s"]) > 0, &l);
        }

        // PUT replaces the whole set.
        if let Some(p) = self.expect("pin replacement answers", "PUT", &path, Some(json!({"snapshots": [first], "ttl_s": PIN_TTL_MAX_S})), 200) {
            self.r.check("a pin may last a day", num(&p["expires_in_s"]) > 0 && num(&p["expires_in_s"]) <= PIN_TTL_MAX_S as i64, &p);
        }
        if let Some(l) = self.expect("replaced pins list answers", "GET", &path, None, 200) {
            self.r.check("PUT replaces the owner's whole set", pinned_ids(&l) == [first.clone()], &l);
        }
        if let Some(s) = self.expect("dropped pin snapshot info answers", "GET", &format!("/council/v2/snapshots/{}", head), None, 200) {
            self.r.check("a snapshot dropped from the set names no owner", s["pins"].as_array().is_some_and(|p| p.iter().all(|x| x["owner"] != json!(owner))), &s);
        }

        for ttl in [0u64, PIN_TTL_MAX_S + 1] {
            self.refused(&format!("ttl_s {} is refused", ttl), "PUT", &path, Some(json!({"snapshots": [], "ttl_s": ttl})), 400);
        }
        self.refused("a pin body with an unknown member is refused", "PUT", &path, Some(json!({"snapshots": [], "ttl_s": 5, "extra": 1})), 400);
        self.refused("an owner of 129 characters is refused", "PUT", &format!("/council/v2/pins/{}", "k".repeat(129)), Some(json!({"snapshots": [], "ttl_s": 5})), 400);

        // An empty set holds nothing.
        if let Some(p) = self.expect("an empty pin set answers", "PUT", &path, Some(json!({"snapshots": [], "ttl_s": 60})), 200) {
            self.r.check("an empty set has no time", p["expires_in_s"].is_null() && p["snapshots"] == json!([]), &p);
            self.refused("an owner with an empty set is a 404", "GET", &path, None, 404);
        }

        self.expect("pin before delete answers", "PUT", &path, Some(json!({"snapshots": [head], "ttl_s": 60})), 200);
        let status = self.send("unpin answers", "DELETE", &path, None);
        if let Some((st, _)) = status {
            self.r.check("unpin is a 204", st == 204, st);
        }
        self.refused("an unpinned owner is a 404", "GET", &path, None, 404);
    }
}

fn pinned_ids(v: &Value) -> Vec<String> {
    v["snapshots"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().or_else(|| e["id"].as_str()).map(String::from)).collect()).unwrap_or_default()
}
