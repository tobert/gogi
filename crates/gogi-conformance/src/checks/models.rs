use gogi_proto::identity::{Capability, ModelStatus};
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    /// Sends a request as written, without the checked served model added. A transport failure is
    /// recorded.
    fn bare(&mut self, what: &str, path: &str, body: Value) -> Option<(u16, Value)> {
        match self.http.call("POST", path, Some(&body)) {
            Ok(r) => Some(r),
            Err(e) => {
                self.r.fail(what, e);
                None
            }
        }
    }

    /// A request names its served model, and the server routes it there or refuses it.
    pub(crate) fn models(&mut self) {
        let (Some(id), Some(me)) = (self.identity.clone(), self.model.clone()) else { return };
        let ctx = self.ctx(self.opening("models"));
        let named = |model: &str| json!({"model": model, "context": ctx});
        let Some(b) = self.expect("a build naming the served model succeeds", "POST", "/council/v2/contexts", Some(named(&me.model)), 200) else { return };
        self.r.check("a build names its served model", b["model"] == json!(me.model), &b["model"]);

        if let Some((status, v)) = self.bare("a build without model", "/council/v2/contexts", json!({"context": ctx})) {
            if id.models.len() == 1 {
                self.r.check("with one served model, a build may leave out model", status == 200 && v["model"] == json!(me.model) && v["head"] == b["head"], (status, &v));
            } else {
                self.r.check("with more than one served model, a build without model is a 400 naming model", status == 400 && v["error"]["param"] == "model", (status, &v));
            }
        }
        let unknown = format!("gogi-conformance-no-such-model-{}", self.nonce);
        if let Some((status, v)) = self.bare("a build naming no served model", "/council/v2/contexts", named(&unknown)) {
            self.r.check("a build naming no served model is a 400 naming model", status == 400 && v["error"]["param"] == "model", (status, &v));
        }
        for alias in &me.aliases {
            if let Some((status, v)) = self.bare("a build naming an alias", "/council/v2/contexts", named(alias)) {
                self.r.check(&format!("an alias, {}, routes to its served model and is answered with the name", alias), status == 200 && v["model"] == json!(me.model) && v["head"] == b["head"], (status, &v));
            }
        }
        if me.aliases.is_empty() {
            self.r.skip("an alias routes to its served model", "the checked served model has no aliases");
        }

        // Served models that are not ready refuse before anything runs. A status can change between
        // reading identity and sending the request, so the check names the status it expected.
        let waiting: Vec<_> = id.models.iter().filter(|m| matches!(m.status, ModelStatus::Loading | ModelStatus::Draining | ModelStatus::Unloaded)).cloned().collect();
        for m in &waiting {
            if let Some((status, v)) = self.bare("a build naming a served model that is not ready", "/council/v2/contexts", named(&m.model)) {
                let e = &v["error"];
                self.r.check(
                    &format!("{}, {:?} in identity, is a 503 model_unavailable naming it and its status", m.model, m.status),
                    status == 503 && e["type"] == "model_unavailable" && e["model"] == json!(m.model) && e["status"].as_str().is_some_and(|s| s != "ready"),
                    (status, &v),
                );
            }
        }
        if waiting.is_empty() {
            self.r.skip("a served model that is not ready is a 503", "every served model is ready");
        }

        // Two served models never share a snapshot.
        let others: Vec<_> = id.models.iter().filter(|m| m.status == ModelStatus::Ready && m.model != me.model).cloned().collect();
        for m in &others {
            if let Some((status, v)) = self.bare("a build on another ready served model", "/council/v2/contexts", named(&m.model)) {
                self.r.check(&format!("{} builds the same context into a snapshot of its own", m.model), status == 200 && v["model"] == json!(m.model) && v["head"].is_string() && v["head"] != b["head"], (status, &v));
            }
        }
        if others.is_empty() {
            self.r.skip("two served models never share a snapshot", "only one served model is ready");
        }

        self.classes(&ctx);
    }

    /// `class` and `priority` never change a number, and a class the contract does not name is a 400.
    fn classes(&mut self, ctx: &Value) {
        let read = |extra: Value| {
            let mut r = json!({"contexts": [ctx], "suffixes": ["\nAnswer: "], "letters": ["A", "B", "C"], "top": 4});
            for (k, v) in extra.as_object().into_iter().flatten() {
                r[k] = v.clone();
            }
            r
        };
        if self.has(Capability::Reads) {
            let plain = self.expect("a read without class succeeds", "POST", "/council/v2/reads", Some(read(json!({}))), 200);
            let batch = self.expect("a batch read with a priority succeeds", "POST", "/council/v2/reads", Some(read(json!({"class": "batch", "priority": -5}))), 200);
            if let (Some(a), Some(b)) = (plain, batch) {
                self.r.check("class and priority do not change a read's numbers", a["reads"] == b["reads"], (&a["reads"], &b["reads"]));
            }
        } else {
            self.r.skip("class and priority do not change a read's numbers", "the served model does not declare the reads capability");
        }
        let build = |extra: Value| {
            let mut b = json!({"context": ctx});
            for (k, v) in extra.as_object().into_iter().flatten() {
                b[k] = v.clone();
            }
            b
        };
        if let Some(b) = self.expect("an interactive build with a priority succeeds", "POST", "/council/v2/contexts", Some(build(json!({"class": "interactive", "priority": 7}))), 200) {
            self.r.check("class and priority do not change a build", b["fed"] == 0, &b);
        }
        self.refused("a class the contract does not name is refused", "POST", "/council/v2/contexts", Some(build(json!({"class": "filler"}))), 400);
        self.refused("a priority outside 32 bits is refused", "POST", "/council/v2/contexts", Some(build(json!({"priority": 1u64 << 31}))), 400);
        self.r.ok_with("the server schedules by class", if self.has(Capability::Classes) { "declared" } else { "not declared; class and priority are accepted and ignored" });
    }
}
