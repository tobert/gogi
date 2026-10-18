use gogi_proto::decision::{DecisionRequest, DecisionResponse};
use gogi_proto::spec::HeldSpec;
use gogi_proto::{math, spec_id_of};
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    pub(crate) fn gate_spec(&self, label: &str) -> Value {
        json!({"name": format!("conformance-{}-{}", label, self.nonce), "instructions": "Judge the proposed command with what this conversation says.",
            "input_label": "Proposed command",
            "questions": [
                {"id": "verdict", "type": "choice", "instructions": "What happens with it.",
                 "criteria": [{"option": "allow", "means": "routine, local, easy to undo"}, {"option": "ask", "means": "outward-facing or hard to undo"}, {"option": "report", "means": "could destroy work"}]},
                {"id": "undo", "type": "score", "instructions": "How hard it is to take back.", "criteria": ["easy", "hard", "impossible"]},
                {"id": "risky", "type": "noul", "instructions": "The command could lose work."}]})
    }

    pub(crate) fn specs(&mut self) {
        let spec = self.gate_spec("specs");
        let Some(a) = self.expect("spec post succeeds", "POST", "/council/v2/specs", Some(spec.clone()), 200) else { return };
        let Some(held) = self.decode::<HeldSpec>("spec response has the contract's fields", &a) else { return };
        let id = held.spec_id.to_string();
        self.r.check("a spec's id is the hash of its canonical JSON", spec_id_of(&spec).ok().as_deref() == Some(id.as_str()), (&id, spec_id_of(&spec)));
        self.r.check("a spec is returned as sent", a["spec"] == spec, &a["spec"]);

        // The same content in another member order is the same spec.
        let mut members: Vec<(String, Value)> = spec.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        members.reverse();
        let reversed: Value = Value::Object(members.into_iter().collect());
        if let Some(b) = self.expect("spec post in another member order succeeds", "POST", "/council/v2/specs", Some(reversed), 200) {
            self.r.check("a spec is held by content, whatever the member order", b["spec_id"] == a["spec_id"], (&a["spec_id"], &b["spec_id"]));
        }
        let mut other = spec.clone();
        other["instructions"] = json!("Judge it differently.");
        if let Some(c) = self.expect("a different spec succeeds", "POST", "/council/v2/specs", Some(other.clone()), 200) {
            self.r.check("different content is a different spec", c["spec_id"] != a["spec_id"] && c["spec_id"] == json!(spec_id_of(&other).unwrap_or_default()), &c["spec_id"]);
            if let Some(id) = c["spec_id"].as_str() {
                self.expect("a spec deletes", "DELETE", &format!("/council/v2/specs/{}", id), None, 204);
                self.refused("a deleted spec is a 404", "GET", &format!("/council/v2/specs/{}", id), None, 404);
            }
        }
        if let Some(g) = self.expect("spec get succeeds", "GET", &format!("/council/v2/specs/{}", id), None, 200) {
            self.r.check("GET returns a held spec as posted", g["spec_id"] == a["spec_id"] && g["spec"] == spec, &g);
        }
        if let Some(l) = self.expect("spec list succeeds", "GET", "/council/v2/specs", None, 200) {
            self.r.check("the spec list includes the spec", l["specs"].as_array().is_some_and(|s| s.iter().any(|x| x["spec_id"] == a["spec_id"])), "not listed");
        }
        for (name, edit) in [
            ("a spec with an unknown member", json!({"extra": 1})),
            ("a spec with no questions", json!({"questions": []})),
            ("a spec with a fraction", json!({"questions": [{"id": "v", "type": "noul", "instructions": {"weight": 1.5}}]})),
            ("a spec with one option in a choice", json!({"questions": [{"id": "v", "type": "choice", "instructions": "x", "criteria": [{"option": "a", "means": "b"}]}]})),
        ] {
            let mut bad = spec.clone();
            for (k, v) in edit.as_object().unwrap() {
                bad[k] = v.clone();
            }
            self.refused(&format!("{} is refused", name), "POST", "/council/v2/specs", Some(bad), 400);
        }
        self.refused("an unknown spec is a 404", "GET", &format!("/council/v2/specs/sha256:{}", "0".repeat(64)), None, 404);
        self.expect("spec cleanup", "DELETE", &format!("/council/v2/specs/{}", id), None, 204);
    }

    /// Posts the suite's spec (`verdict`, `undo`, `risky`) and returns its id.
    pub(crate) fn hold_gate(&mut self, label: &str) -> Option<String> {
        let spec = self.gate_spec(label);
        let a = self.expect("spec post succeeds", "POST", "/council/v2/specs", Some(spec), 200)?;
        a["spec_id"].as_str().map(String::from)
    }

    pub(crate) fn decisions(&mut self) {
        let Some(spec_id) = self.hold_gate("decisions") else { return };
        let msgs = self.opening("decision");
        let other = vec![json!({"role": "system", "content": format!("You are a careful reviewer ({}).", self.nonce)}), json!({"role": "user", "content": "Never push to a shared remote.", "snap": true})];
        let (c0, c1) = (self.ctx(msgs), self.ctx(other));
        let req = json!({"spec_id": spec_id, "state": "git push origin main", "contexts": [c0, c1], "pool": {"method": "loglinear", "weights": "mass"}});
        let Some(d) = self.expect("decision succeeds", "POST", "/council/v2/decisions", Some(req.clone()), 200) else { return };
        let Some(resp) = self.decode::<DecisionResponse>("decision response has the contract's fields", &d) else { return };
        let Ok(parsed) = serde_json::from_value::<DecisionRequest>(req.clone()) else { return };
        let verified = math::verify(&resp, &parsed.expect());
        self.r.check("decision numbers obey the exact relations", verified.is_ok(), verified.as_ref().err().map(|m| m.to_string()));
        self.r.check("each read names its context by index", resp.reads.iter().map(|r| r.context.clone()).collect::<Vec<_>>() == [Some("0".to_string()), Some("1".to_string())], d["reads"].as_array().map(|r| r.iter().map(|x| x["context"].clone()).collect::<Vec<_>>()));
        self.r.check("each read names the snapshot it read after", resp.reads.iter().all(|r| r.snapshot.is_some()), &d["reads"]);
        let loo_keys = |q: &str| d["answers"][q]["leave_one_out"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>());
        if self.has(gogi_proto::identity::Capability::LeaveOneOut) {
            self.r.check("leave_one_out is keyed by context index", loo_keys("verdict") == Some(vec!["0".into(), "1".into()]), loo_keys("verdict"));
        }
        self.r.check("every asked question is answered", ["verdict", "undo", "risky"].iter().all(|q| d["answers"][q].is_object() && d["reads"][0]["answers"][q].is_object()), &d["answers"]);
        self.r.check("a choice names an option of the spec", ["allow", "ask", "report"].contains(&d["answers"]["verdict"]["choice"].as_str().unwrap_or("")), &d["answers"]["verdict"]);
        self.r.check("pool echoes its settings and one weight per read", d["pool"]["method"] == json!("loglinear") && d["pool"]["weights"] == json!("mass") && d["pool"]["normalized"]["verdict"].as_array().is_some_and(|w| w.len() == 2), &d["pool"]);
        self.r.check("identity names the spec", d["identity"]["spec_id"] == json!(spec_id), &d["identity"]);
        if let Some(m) = self.model.clone() {
            let same = m.identity().iter().all(|(k, v)| d["identity"][*k] == json!(v));
            self.r.check("a decision's identity matches its served model's", same && d["model"] == json!(m.model), (&d["identity"], m.identity()));
        }

        // Replay is bit for bit, whatever ran between.
        let g = self.ctx(vec![json!({"role": "system", "content": format!("Unrelated {}.", self.nonce)}), json!({"role": "user", "content": "Say hi.", "snap": true})]);
        if self.has(gogi_proto::identity::Capability::Generate) {
            self.generate("an unrelated generation succeeds", &g, 8);
        }
        if let Some(d2) = self.expect("decision again succeeds", "POST", "/council/v2/decisions", Some(req.clone()), 200) {
            let same = (0..2).all(|i| d["reads"][i]["answers"] == d2["reads"][i]["answers"] && d["reads"][i]["snapshot"] == d2["reads"][i]["snapshot"]);
            self.r.check("a decision replays bit for bit", same && d["answers"] == d2["answers"], "the reads or pooled answers changed");
        }

        // Without a context, one read with no context.
        let none = json!({"spec_id": spec_id, "state": {"command": "ls", "cwd": "/tmp"}});
        if let Some(p) = self.expect("a decision with no context succeeds", "POST", "/council/v2/decisions", Some(none.clone()), 200) {
            self.r.check("a decision with no context has one read with context null", p["reads"].as_array().is_some_and(|r| r.len() == 1 && r[0]["context"].is_null()), &p["reads"]);
            if let (Some(resp), Ok(req)) = (self.decode::<DecisionResponse>("a decision with no context decodes", &p), serde_json::from_value::<DecisionRequest>(none)) {
                let v = math::verify(&resp, &req.expect());
                self.r.check("a decision with no context obeys the exact relations", v.is_ok(), v.err().map(|m| m.to_string()));
            }
            self.r.check("one read has no leave_one_out", p["answers"]["verdict"].get("leave_one_out").is_none(), &p["answers"]["verdict"]);
        }

        // ask and options narrow.
        let ctx0 = self.ctx(self.opening("decision"));
        let narrow = json!({"spec_id": spec_id, "state": "ls", "contexts": [ctx0], "ask": ["verdict"], "options": {"verdict": ["ask", "allow"]}});
        if let Some(n) = self.expect("a narrowed decision succeeds", "POST", "/council/v2/decisions", Some(narrow.clone()), 200) {
            let keys: Vec<&str> = n["answers"]["verdict"]["probabilities"].as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default();
            self.r.check("ask limits the questions, and options limits a choice's options", n["answers"].as_object().is_some_and(|a| a.len() == 1) && keys.len() == 2 && keys.contains(&"ask") && keys.contains(&"allow"), &n["answers"]);
            if let (Some(resp), Ok(req)) = (self.decode::<DecisionResponse>("a narrowed decision decodes", &n), serde_json::from_value::<DecisionRequest>(narrow)) {
                let v = math::verify(&resp, &req.expect());
                self.r.check("a narrowed decision obeys the exact relations", v.is_ok(), v.err().map(|m| m.to_string()));
            }
        }

        // Inline questions.
        let inline = json!({"questions": {"verdict": {"type": "choice", "instructions": "Fine to run?", "criteria": {"yes": "routine", "no": "not routine"}}}, "state": "ls"});
        if let Some(i) = self.expect("an inline-question decision succeeds", "POST", "/council/v2/decisions", Some(inline), 200) {
            self.r.check("inline questions answer in the order listed", i["answers"]["verdict"]["probabilities"].as_object().is_some_and(|m| m.keys().collect::<Vec<_>>() == ["yes", "no"]), &i["answers"]);
        }

        self.queue_decisions(&spec_id);

        let c = self.ctx(self.opening("decision"));
        let bad: Vec<(&str, Value, u16)> = vec![
            ("a decision with a v1 context reference", json!({"spec_id": spec_id, "state": "x", "contexts": [{"id": "old-style"}]}), 400),
            ("a decision with an unknown member", json!({"spec_id": spec_id, "state": "x", "contxts": []}), 400),
            ("a decision with both spec_id and questions", json!({"spec_id": spec_id, "state": "x", "questions": {"q": {"type": "noul", "instructions": "x"}}}), 400),
            ("a decision with neither spec_id nor questions", json!({"state": "x"}), 400),
            ("a decision with a number for state", json!({"spec_id": spec_id, "state": 3}), 400),
            ("a decision with timeout_ms 0", json!({"spec_id": spec_id, "state": "x", "timeout_ms": 0}), 400),
            ("a decision for another model", json!({"spec_id": spec_id, "state": "x", "model": "no-such-model-gogi"}), 400),
            ("a decision with nine contexts", json!({"spec_id": spec_id, "state": "x", "contexts": vec![c.clone(); 9]}), 400),
            ("a decision asking an unknown question", json!({"spec_id": spec_id, "state": "x", "ask": ["nope"]}), 400),
            ("a decision narrowing to one option", json!({"spec_id": spec_id, "state": "x", "options": {"verdict": ["allow"]}}), 400),
            ("a decision with given weights and no values", json!({"spec_id": spec_id, "state": "x", "pool": {"weights": "given"}}), 400),
            ("a decision with values and uniform weights", json!({"spec_id": spec_id, "state": "x", "pool": {"values": [1.0]}}), 400),
            ("a decision with given weights that sum to 0", json!({"spec_id": spec_id, "state": "x", "pool": {"weights": "given", "values": [0.0]}}), 400),
            ("a decision on a bad context", json!({"spec_id": spec_id, "state": "x", "contexts": [self.ctx(vec![json!({"role": "tool", "tool_call_id": "z", "content": "x"})])]}), 400),
            ("a decision on a spec the server does not hold", json!({"spec_id": format!("sha256:{}", "0".repeat(64)), "state": "x"}), 404),
        ];
        for (name, body, want) in bad {
            self.refused(&format!("{} is refused", name), "POST", "/council/v2/decisions", Some(body), want);
        }
        if !self.has(gogi_proto::identity::Capability::Think) {
            self.refused("think above 0 tokens needs the think capability", "POST", "/council/v2/decisions", Some(json!({"spec_id": spec_id, "state": "x", "think": {"tokens": 8}})), 400);
        }
        if let Some(t) = self.send("think 0 succeeds", "POST", "/council/v2/decisions", Some(json!({"spec_id": spec_id, "state": "x", "think": {"tokens": 0}}))) {
            self.r.check("think with 0 tokens needs no think capability", t.0 == 200, t);
        }

        // given weights.
        let (c0, c1) = (self.ctx(self.opening("decision")), self.ctx(vec![json!({"role": "user", "content": "Be brief.", "snap": true})]));
        let given = json!({"spec_id": spec_id, "state": "ls", "contexts": [c0, c1], "pool": {"method": "linear", "weights": "given", "values": [3.0, 1.0]}});
        if let Some(gv) = self.expect("a decision with given weights succeeds", "POST", "/council/v2/decisions", Some(given.clone()), 200) {
            if let (Some(resp), Ok(req)) = (self.decode::<DecisionResponse>("a given-weight decision decodes", &gv), serde_json::from_value::<DecisionRequest>(given)) {
                let v = math::verify(&resp, &req.expect());
                self.r.check("given weights are normalized and pooled in request order", v.is_ok() && gv["pool"]["normalized"]["verdict"] == json!([0.75, 0.25]), v.err().map(|m| m.to_string()));
            }
        }
        self.expect("spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }

    /// A context that ends with a call to its queue tool receives the case as that call's result.
    fn queue_decisions(&mut self, spec_id: &str) {
        let schema = |letters: &[&str]| {
            json!({"type": "function", "function": {"name": "next_command", "description": "The next command to judge.",
              "parameters": {"type": "object", "properties": {"answers": {"type": "object", "properties": {
                "verdict": {"type": "string", "enum": ["A", "B", "C"]},
                "undo": {"type": "string", "enum": letters},
                "risky": {"type": "string", "enum": ["A", "B"]}}}}}}})
        };
        let queue_ctx = |s: &Suite, letters: &[&str]| {
            json!({"thinking": true, "tools": [schema(letters)], "queue": "next_command", "messages": [
                {"role": "system", "content": format!("You judge shell commands ({}).", s.nonce)},
                {"role": "user", "content": "Commands wait in a queue. Fetch the first one.", "snap": true},
                {"role": "assistant", "tool_calls": [{"id": "q0", "name": "next_command", "arguments": {}}]}]})
        };
        let good = queue_ctx(self, &["A", "B", "C"]);
        let req = json!({"spec_id": spec_id, "state": "rm -rf build", "contexts": [good.clone()]});
        let Some((status, d)) = self.send("a queue decision succeeds", "POST", "/council/v2/decisions", Some(req.clone())) else { return };
        if status == 400 && d["error"]["param"] == json!("queue") {
            self.r.skip("queue decisions", format!("the server does not deliver a case through a queue tool: {}", d["error"]["message"]));
            return;
        }
        self.r.check("a queue decision succeeds", status == 200, (status, &d));
        if status != 200 {
            return;
        }
        if let (Some(resp), Ok(parsed)) = (self.decode::<DecisionResponse>("a queue decision decodes", &d), serde_json::from_value::<DecisionRequest>(req.clone())) {
            let v = math::verify(&resp, &parsed.expect());
            self.r.check("a queue decision obeys the exact relations", v.is_ok(), v.err().map(|m| m.to_string()));
        }
        let (_, again) = self.send("a queue decision again succeeds", "POST", "/council/v2/decisions", Some(req)).unwrap_or((0, Value::Null));
        self.r.check("a queue decision replays bit for bit", again["reads"][0]["answers"] == d["reads"][0]["answers"], &again["reads"]);
        let wrong = queue_ctx(self, &["A", "B"]);
        if let Some(e) = self.refused("a queue tool whose answers schema is wrong is refused", "POST", "/council/v2/decisions", Some(json!({"spec_id": spec_id, "state": "x", "contexts": [wrong]})), 400) {
            self.r.check("the error's param is queue", e["error"]["param"] == json!("queue"), &e);
        }
        let other = self.ctx(self.opening("decision"));
        self.refused("a decision that mixes queue contexts and other contexts is refused", "POST", "/council/v2/decisions", Some(json!({"spec_id": spec_id, "state": "x", "contexts": [good, other]})), 400);
    }
}
