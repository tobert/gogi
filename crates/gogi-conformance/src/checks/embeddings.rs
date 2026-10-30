use gogi_proto::embeddings::{EmbeddingsResponse, cut, norm};
use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

const PATH: &str = "/council/v2/embeddings";

/// How far a vector's L2 norm may be from 1: float32 rounding of a unit vector.
const NORM_TOLERANCE: f64 = 1e-4;

/// How far a component of a cut may be from the whole vector cut and normalized again.
const CUT_TOLERANCE: f32 = 1e-5;

/// The vectors of a response, in input order. A missing vector is empty.
fn vectors(r: &EmbeddingsResponse) -> Vec<Vec<f32>> {
    r.embeddings.iter().map(|e| e.vector.clone().unwrap_or_default()).collect()
}

impl Suite {
    /// Sends an embedding request and decodes its `200` response. A failure is recorded.
    fn embed(&mut self, what: &str, body: Value) -> Option<EmbeddingsResponse> {
        let v = self.expect(what, "POST", PATH, Some(body), 200)?;
        match serde_json::from_value::<EmbeddingsResponse>(v.clone()) {
            Ok(r) => Some(r),
            Err(e) => {
                self.r.fail(what, format!("the response does not decode: {}: {}", e, v));
                None
            }
        }
    }

    /// Checks `POST /council/v2/embeddings` on the checked served model: the shape, unit
    /// vectors, replay, independence from the other inputs, the task, the cuts, the dry run, the
    /// limit, and each refusal. A served model without `embed` refuses naming `model`.
    pub(crate) fn embeddings(&mut self) {
        let Some(me) = self.model.clone() else { return };
        if !self.has(Capability::Embed) {
            if let Some((status, v)) = self.send("embeddings to a served model without embed", "POST", PATH, Some(json!({"task": "query", "inputs": ["x"]}))) {
                self.r.check("embeddings to a served model without embed is a 400 naming model", status == 400 && v["error"]["param"] == json!("model"), (status, &v));
            }
            self.r.skip("embeddings", "the served model does not declare embed");
            return;
        }
        let Some(e) = me.embeddings.clone() else {
            self.r.fail("a served model that declares embed has embeddings", "identity has no embeddings member for it");
            return;
        };
        let task = if e.tasks.iter().any(|t| t == "query") { "query".to_string() } else { e.tasks[0].clone() };
        let largest = e.dimensions.iter().copied().max().unwrap_or(0);
        let a = format!("pin the snapshot for an hour {}", self.nonce);
        let b = format!("evict the oldest snapshot first {}", self.nonce);

        let Some(r) = self.embed("an embedding request succeeds", json!({"task": task, "inputs": [a, b]})) else { return };
        self.r.check("an embeddings response names its served model, task, and dimensions", r.model == me.model && r.task == task && r.dimensions == largest, (&r.model, &r.task, r.dimensions));
        let indexes: Vec<u64> = r.embeddings.iter().map(|x| x.index).collect();
        self.r.check("there is one embedding for each input, in input order", indexes == [0, 1], &indexes);
        self.r.check("each embedding counts its tokens", r.embeddings.iter().all(|x| x.tokens > 0), &indexes);
        let vs = vectors(&r);
        self.r.check("each vector has dimensions components", vs.iter().all(|v| v.len() == largest as usize), vs.iter().map(Vec::len).collect::<Vec<_>>());
        let norms: Vec<f64> = vs.iter().map(|v| norm(v)).collect();
        self.r.check("each vector has an L2 norm of 1", norms.iter().all(|n| (n - 1.0).abs() < NORM_TOLERANCE), &norms);
        let tokens: u64 = r.embeddings.iter().map(|x| x.tokens).sum();
        let u = &r.usage;
        self.r.check("usage counts each input's tokens as prompt and fed", u.prompt == tokens && u.fed == tokens && u.kept == 0 && u.completion == 0 && u.reasoning == 0, u);
        let id = &r.identity;
        let want: Vec<(&str, &str)> = me.identity().to_vec();
        let got = [("model", id.model.as_str()), ("weight_hash", id.weight_hash.as_str()), ("tokenizer_hash", id.tokenizer_hash.as_str()), ("template", id.template.as_str()), ("engine", id.engine.as_str())];
        self.r.check("an embeddings response carries the served model's identity", got.to_vec() == want, (&got, &want));

        if let Some(again) = self.embed("the same embedding request again succeeds", json!({"task": task, "inputs": [a, b]})) {
            self.r.check("the same request gives the same vectors bit for bit", vectors(&again) == vs, "");
        }
        if let Some(alone) = self.embed("an input embedded alone succeeds", json!({"task": task, "inputs": [b]})) {
            self.r.check("an input's vector does not depend on the other inputs", vectors(&alone).first() == vs.get(1), "");
        }
        if let Some(swapped) = self.embed("inputs in another order succeed", json!({"task": task, "inputs": [b, a]})) {
            let sv = vectors(&swapped);
            self.r.check("an input's vector does not depend on the order of the inputs", sv.first() == vs.get(1) && sv.get(1) == vs.first(), "");
        }
        match e.tasks.iter().find(|t| **t != task).cloned() {
            Some(other) => {
                if let Some(o) = self.embed("an embedding request with another task succeeds", json!({"task": other, "inputs": [a]})) {
                    self.r.check(&format!("the task changes the vector: {} and {}", task, other), vectors(&o).first() != vs.first(), "");
                }
            }
            None => self.r.skip("the task changes the vector", "the served model has one task"),
        }

        let cuts: Vec<u32> = e.dimensions.iter().copied().filter(|d| *d != largest).collect();
        for d in &cuts {
            if let Some(c) = self.embed(&format!("an embedding request with {} dimensions succeeds", d), json!({"task": task, "inputs": [a], "dimensions": d})) {
                let got = vectors(&c).first().cloned().unwrap_or_default();
                let want = cut(&vs[0], *d as usize);
                let worst = got.iter().zip(&want).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
                self.r.check(
                    &format!("a vector of {} dimensions is the whole vector cut and normalized again", d),
                    c.dimensions == *d && got.len() == want.len() && worst < CUT_TOLERANCE,
                    format!("dimensions {}, {} components, largest difference {}", c.dimensions, got.len(), worst),
                );
            }
        }
        if cuts.is_empty() {
            self.r.skip("a shorter vector is the whole vector cut and normalized again", "the served model offers one length");
        }

        // An input past context_tokens: each number is at least one token in any tokenizer the suite knows.
        let limit = me.limits.context_tokens;
        let long: String = (0..=limit).map(|n| n.to_string()).collect::<Vec<_>>().join(" ");
        if self.has(Capability::DryRun) {
            if let Some(dry) = self.embed("a dry run of embeddings succeeds", json!({"task": task, "inputs": [a, long], "dry_run": true})) {
                let none = dry.embeddings.iter().all(|x| x.vector.is_none());
                let counts: Vec<u64> = dry.embeddings.iter().map(|x| x.tokens).collect();
                self.r.check("a dry run counts each input's tokens and embeds nothing", none && dry.usage.fed == 0 && counts.first() == r.embeddings.first().map(|x| x.tokens).as_ref(), (&counts, dry.usage.fed));
                self.r.check("a dry run counts an input past context_tokens", counts.get(1).is_some_and(|n| *n > limit), (&counts, limit));
            }
        } else {
            self.r.skip("a dry run counts each input's tokens and embeds nothing", "the served model does not declare dry_run");
        }
        if let Some((status, v)) = self.send("an input past context_tokens", "POST", PATH, Some(json!({"task": task, "inputs": [a, long]}))) {
            let err = &v["error"];
            self.r.check(
                "an input past context_tokens is a 413 too_large naming context_tokens, with both counts",
                status == 413 && err["type"] == "too_large" && err["param"] == "context_tokens" && err["context_tokens"] == json!(limit) && err["prompt_tokens"].as_u64().is_some_and(|n| n > limit),
                (status, &v),
            );
        }

        let many: Vec<String> = (0..=e.inputs_per_request).map(|n| format!("input {} {}", n, self.nonce)).collect();
        let refusals: Vec<(&str, Value, &str)> = vec![
            ("a task the served model does not list", json!({"task": format!("gogi-conformance-no-such-task-{}", self.nonce), "inputs": [a]}), "task"),
            ("no inputs", json!({"task": task, "inputs": []}), "inputs"),
            ("more than inputs_per_request inputs", json!({"task": task, "inputs": many}), "inputs"),
            ("an empty input", json!({"task": task, "inputs": [a, ""]}), "inputs[1]"),
            ("dimensions the served model does not offer", json!({"task": task, "inputs": [a], "dimensions": largest + 1}), "dimensions"),
            ("an unknown member", json!({"task": task, "inputs": [a], "encoding_format": "float"}), "encoding_format"),
        ];
        for (what, body, param) in refusals {
            if let Some((status, v)) = self.send(&format!("embeddings with {}", what), "POST", PATH, Some(body)) {
                self.r.check(&format!("embeddings with {} is a 400 naming {}", what, param), status == 400 && v["error"]["param"] == json!(param), (status, &v));
            }
        }
        if let Some((status, v)) = self.send_as_written("embeddings without model", "POST", PATH, Some(&json!({"task": task, "inputs": [a]}))) {
            self.r.check("embeddings without model is a 400 naming model", status == 400 && v["error"]["param"] == json!("model"), (status, &v));
        }
        if me.inputs().is_empty() {
            let part = json!([{"type": "object", "id": format!("blake3:{}", "0".repeat(64)), "mime": "image/png"}]);
            if let Some((status, v)) = self.send("an object part to a model that embeds text only", "POST", PATH, Some(json!({"task": task, "inputs": [part]}))) {
                self.r.check("an object part to a model that embeds text only is a 400 naming inputs[0][0]", status == 400 && v["error"]["param"] == json!("inputs[0][0]"), (status, &v));
            }
        }
    }

    /// A served model that only embeds refuses every request that reads a context, naming `model`.
    pub(crate) fn embeds_only(&mut self) {
        let ctx = self.ctx(self.opening("embeds-only"));
        let spec = format!("blake3:{}", "0".repeat(64));
        for (what, path, body) in [
            ("a build", "/council/v2/contexts", json!({"context": ctx})),
            ("a decision", "/council/v2/decisions", json!({"spec_id": spec, "case": "x", "contexts": [ctx]})),
            ("a raw read", "/council/v2/reads", json!({"contexts": [ctx], "suffixes": ["A"], "letters": ["A", "B"]})),
            ("a generation", "/council/v2/generations", json!({"context": ctx, "max_tokens": 4})),
        ] {
            let name = format!("{} naming a model that only embeds is a 400 naming model", what);
            if let Some((status, v)) = self.send(&name, "POST", path, Some(body)) {
                self.r.check(&name, status == 400 && v["error"]["param"] == json!("model"), (status, &v));
            }
        }
    }
}
