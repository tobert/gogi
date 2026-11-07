use gogi_proto::content_id;
use gogi_proto::control::{ControlVector, ControlVectorResponse, MEDIA_TYPE};
use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

const PATH: &str = "/council/v2/control-vectors";

/// A control item.
fn item(vector: &str, op: &str, strength: f64) -> Value {
    json!({"vector": vector, "op": op, "strength": strength})
}

impl Suite {
    /// A closed context of a user message and an assistant reply, unique to this run.
    fn said(&self, reply: &str) -> Value {
        let mut c = self.ctx(vec![
            json!({"role": "system", "content": format!("You write stories ({} control).", self.nonce)}),
            json!({"role": "user", "content": "Write the scene where the dragon wins.", "snap": true}),
            json!({"role": "assistant", "content": reply}),
        ]);
        c.as_object_mut().expect("a context is an object").remove("tools");
        c
    }

    /// A context with `control`, unique to this run.
    fn steered(&self, control: Value) -> Value {
        let mut c = self.ctx(vec![json!({"role": "user", "content": format!("Tell the story of the dragon ({}).", self.nonce), "snap": true})]);
        c["control"] = control;
        c
    }

    /// The numbers a context gives: a raw read's letter log probabilities with `reads`, or else a
    /// greedy reply with `generate`. `None` when the served model has neither or the request fails.
    fn numbers(&mut self, what: &str, ctx: &Value) -> Option<Value> {
        if self.has(Capability::Reads) {
            let body = json!({"contexts": [ctx], "suffixes": ["\nAnswer: "], "letters": ["A", "B", "C"]});
            return self.expect(what, "POST", "/council/v2/reads", Some(body), 200).map(|r| r["reads"][0]["logprobs"].clone());
        }
        if self.has(Capability::Generate) {
            return self.generate(what, ctx, 16).map(|g| json!([g["content"], g["reasoning"], g["tool_calls"]]));
        }
        None
    }

    /// Checks control vectors on the checked served model: the route, the object it puts, the
    /// item rules, the relation of `add` and `ablate` at 0 to the plain model, and model state of
    /// its own. A served model without `control_vectors` refuses a `control` naming `control` and
    /// the route naming `model`.
    pub(crate) fn control(&mut self) {
        let any = format!("blake3:{}", "0".repeat(64));
        if !self.has(Capability::ControlVectors) {
            let c = self.steered(json!([item(&any, "add", 0.0)]));
            if let Some(e) = self.refused("a build with control to a model without control_vectors is refused", "POST", "/council/v2/contexts", Some(json!({"context": c})), 400) {
                self.r.check("a build with control to a model without control_vectors is a 400 naming control", e["error"]["param"] == json!("control"), &e);
            }
            if self.has(Capability::Generate)
                && let Some(e) = self.refused("a generation with control to a model without control_vectors is refused", "POST", "/council/v2/generations", Some(json!({"context": c, "max_tokens": 4})), 400)
            {
                self.r.check("a generation with control to a model without control_vectors is a 400 naming control", e["error"]["param"] == json!("control"), &e);
            }
            let body = json!({"positive": [self.said("No.")], "negative": [self.said("Yes.")]});
            if let Some((status, v)) = self.send("a control vector from a model without control_vectors", "POST", PATH, Some(body)) {
                self.r.check("a control vector from a model without control_vectors is a 400 naming model", status == 400 && v["error"]["param"] == json!("model"), (status, &v));
            }
            self.r.skip("control vectors", "the served model does not declare control_vectors");
            return;
        }
        let Some(me) = self.model.clone() else { return };

        // The route.
        let body = json!({"positive": [self.said("I can't write that."), self.said("I won't help with that.")], "negative": [self.said("The dragon lands on the tower.")]});
        let Some(v) = self.expect("a control vector request succeeds", "POST", PATH, Some(body.clone()), 200) else { return };
        let Some(r) = self.decode::<ControlVectorResponse>("a control vector response has the contract's fields", &v) else { return };
        self.r.check("a control vector response names its served model and counts each side", r.model == me.model && (r.contexts.positive, r.contexts.negative) == (2, 1), (&r.model, &r.contexts));
        let id = &r.identity;
        let got = [("model", id.model.as_str()), ("weight_hash", id.weight_hash.as_str()), ("tokenizer_hash", id.tokenizer_hash.as_str()), ("template", id.template.as_str()), ("engine", id.engine.as_str())];
        self.r.check("a control vector response carries the served model's identity", got.to_vec() == me.identity().to_vec(), got);
        self.r.check("a control vector has a layer or more of a width above 0", r.layers > 0 && r.width > 0, (r.layers, r.width));
        self.r.check("a control vector request writes no tokens", r.usage.check().is_ok() && r.usage.completion == 0 && r.usage.reasoning == 0 && r.usage.prompt > 0, r.usage);
        let Some(vector) = r.vector.clone().map(|i| i.to_string()) else {
            self.r.fail("a control vector request returns the vector's id", "vector is null without dry_run");
            return;
        };
        if let Some(again) = self.expect("the same control vector request again succeeds", "POST", PATH, Some(body.clone()), 200) {
            self.r.check("the same contexts give the same vector", again["vector"] == json!(vector), (&again["vector"], &vector));
        }
        let mut width = r.width as usize;
        if self.has(Capability::Objects) {
            if let Some(g) = self.send_bytes("the control vector reads back", "GET", &format!("/council/v2/objects/{}", vector), None, &[]) {
                self.r.check("the control vector reads back with its media type", g.status == 200 && g.header("content-type") == Some(MEDIA_TYPE), (g.status, g.header("content-type")));
                self.r.check("the control vector's bytes hash to its id", content_id(&g.body) == vector, g.body.len());
                match ControlVector::parse(&g.body) {
                    Ok(cv) => {
                        self.r.check("the control vector names its served model and weights", cv.model == me.model && cv.weight_hash == me.weight_hash, (&cv.model, &cv.weight_hash));
                        self.r.check("the control vector has the layers and width the response names", cv.layers.len() == r.layers as usize && cv.width() == width, (cv.layers.len(), cv.width()));
                        width = cv.width();
                    }
                    Err(e) => self.r.fail("the control vector is a safetensors file of the contract's shape", e),
                }
            }
        } else {
            self.r.skip("the control vector reads back", "the server does not declare objects");
        }

        // add and ablate at strength 0 give the plain numbers; any list has model state of its own.
        let plain = self.steered(json!([]));
        let zero = self.steered(json!([item(&vector, "add", 0.0), item(&vector, "ablate", 0.0)]));
        let strong = self.steered(json!([item(&vector, "add", 8.0)]));
        match (self.numbers("the plain context reads", &plain), self.numbers("a context with add and ablate at 0 reads", &zero)) {
            (Some(p), Some(z)) => {
                self.r.check("add and ablate at strength 0 give the numbers of no control", p == z, (&p, &z));
            }
            (None, None) if !self.has(Capability::Reads) && !self.has(Capability::Generate) => self.r.skip("add and ablate at strength 0 give the numbers of no control", "the served model has neither reads nor generate"),
            _ => {}
        }
        if let (Some(p), Some(z), Some(s)) = (self.build("the plain context builds", &plain, false), self.build("a context with control at 0 builds", &zero, false), self.build("a context with add at 8 builds", &strong, false)) {
            let ids = |b: &Value| b["snapshots"].as_array().map(|a| a.iter().map(|x| x["id"].clone()).collect::<Vec<_>>()).unwrap_or_default();
            let shared = |b: &Value| ids(b).iter().any(|i| ids(&p).contains(i)) || ids(&p).contains(&b["from"]);
            self.r.check("a list that changes nothing still has model state of its own", z["head"] != p["head"] && !shared(&z), (&z, &p["head"]));
            self.r.check("a list that changes the model never reuses the plain model state", s["head"] != p["head"] && !shared(&s), (&s, &p["head"]));
        }

        // The item rules.
        let item_refusals: Vec<(&str, Value, &str)> = vec![
            ("an unknown op", json!([item(&vector, "add", 1.0), item(&vector, "boost", 1.0)]), "control[1].op"),
            ("a strength that is not a number", json!([{"vector": vector, "op": "add", "strength": "1"}]), "control[0].strength"),
            ("a short vector id", json!([item(&vector[..19], "add", 1.0)]), "control[0].vector"),
            ("an item with an unknown member", json!([{"vector": vector, "op": "add", "strength": 1, "layer": 2}]), "control[0].layer"),
        ];
        for (what, control, param) in item_refusals {
            let name = format!("a build with {} in control is a 400 naming {}", what, param);
            if let Some(e) = self.refused(&name, "POST", "/council/v2/contexts", Some(json!({"context": self.steered(control.clone())})), 400) {
                self.r.check(&name, e["error"]["param"] == json!(param), &e);
            }
        }
        if self.has(Capability::Reads) {
            let body = json!({"contexts": [plain.clone(), self.steered(json!([item(&vector, "boost", 1.0)]))], "suffixes": ["x"], "letters": ["A", "B"]});
            if let Some(e) = self.refused("raw reads with an unknown op are refused", "POST", "/council/v2/reads", Some(body), 400) {
                self.r.check("raw reads with an unknown op are a 400 naming contexts[1].control[0].op", e["error"]["param"] == json!("contexts[1].control[0].op"), &e);
            }
        }
        let unheld = content_id(format!("no such vector {}", self.nonce).as_bytes());
        if let Some(e) = self.refused("a control vector the server does not hold is refused", "POST", "/council/v2/contexts", Some(json!({"context": self.steered(json!([item(&unheld, "add", 1.0)]))})), 404) {
            self.r.check("a control vector the server does not hold is a 404 naming control[0].vector", e["error"]["param"] == json!("control[0].vector") && e["error"]["message"].as_str().is_some_and(|m| m.contains(&unheld)), &e);
        }
        if self.has(Capability::Objects) {
            let layers = |w: usize| [(0u32, (0..w).map(|j| j as f32 / w as f32 - 0.5).collect::<Vec<f32>>())].into_iter().collect();
            let theirs = ControlVector { model: me.model.clone(), weight_hash: format!("gogi-conformance-other-weights-{}", self.nonce), layers: layers(width) };
            let wide = ControlVector { model: me.model.clone(), weight_hash: me.weight_hash.clone(), layers: layers(width + 1) };
            for (what, cv) in [("a control vector for other weights", theirs), ("a control vector of another width", wide)] {
                let Some(id) = self.put_object(&format!("{} is put", what), &cv.to_bytes(), MEDIA_TYPE) else { continue };
                let name = format!("{} is a 400 naming control[0].vector", what);
                if let Some(e) = self.refused(&name, "POST", "/council/v2/contexts", Some(json!({"context": self.steered(json!([item(&id, "add", 1.0)]))})), 400) {
                    self.r.check(&name, e["error"]["param"] == json!("control[0].vector"), &e);
                }
            }
        }

        // The route's refusals.
        let route_refusals: Vec<(&str, Value, &str)> = vec![
            ("no positive context", json!({"positive": [], "negative": [self.said("Yes.")]}), "positive"),
            ("no negative member", json!({"positive": [self.said("No.")]}), "negative"),
            ("a context with a control of its own", json!({"positive": [self.said("No.")], "negative": [self.said("Yes."), self.steered(json!([item(&vector, "add", 1.0)]))]}), "negative[1].control"),
            ("an unknown member", json!({"positive": [self.said("No.")], "negative": [self.said("Yes.")], "normalize": true}), "normalize"),
        ];
        for (what, body, param) in route_refusals {
            let name = format!("a control vector request with {} is a 400 naming {}", what, param);
            if let Some((status, v)) = self.send(&name, "POST", PATH, Some(body)) {
                self.r.check(&name, status == 400 && v["error"]["param"] == json!(param), (status, &v));
            }
        }
        if let Some(most) = me.limits.control_vector_contexts {
            let many = vec![self.said("No."); most as usize + 1];
            let name = "a side past control_vector_contexts is a 400 naming it";
            if let Some((status, v)) = self.send(name, "POST", PATH, Some(json!({"positive": many, "negative": [self.said("Yes.")]}))) {
                self.r.check(name, status == 400 && v["error"]["param"] == json!("positive"), (status, &v));
            }
        }
        if self.has(Capability::DryRun)
            && let Some(d) = self.expect("a dry run of a control vector request succeeds", "POST", PATH, Some(json!({"positive": [self.said("No.")], "negative": [self.said("Yes.")], "dry_run": true})), 200)
        {
            self.r.check("a dry run of a control vector request makes no vector", d["vector"].is_null() && d["width"] == json!(r.width), &d);
        }
    }
}
