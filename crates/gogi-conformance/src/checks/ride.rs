//! Decisions that ride with a generation (capability `ride`).

use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    pub(crate) fn ride(&mut self) {
        if !self.has(Capability::Ride) {
            self.r.skip("riding decisions", "the server does not declare the ride capability");
            return;
        }
        let Some(spec_id) = self.hold_gate("ride") else { return };
        let ctx = self.ctx(self.opening("ride"));
        let bare = json!({"context": ctx, "max_tokens": 200, "sample": {"temperature": 0}});
        let mut rode = bare.clone();
        rode["decisions"] = json!([{"spec_id": spec_id, "when": "before"}, {"spec_id": spec_id, "when": "after", "ask": ["verdict"]}]);
        let Some(p) = self.expect("a generation without decisions succeeds", "POST", "/council/v2/generations", Some(bare.clone()), 200) else { return };
        let Some(g) = self.expect("a generation with riding decisions succeeds", "POST", "/council/v2/generations", Some(rode), 200) else { return };
        let reply = |v: &Value| (v["content"].clone(), v["reasoning"].clone(), calls(&v["tool_calls"]));
        self.r.check("riding decisions do not change the reply", reply(&p) == reply(&g), (reply(&p), reply(&g)));
        let d = g["decisions"].as_array().cloned().unwrap_or_default();
        self.r.check("each riding decision answers, in request order", d.len() == 2 && d[0]["when"] == json!("before") && d[1]["when"] == json!("after") && d.iter().all(|x| x["spec_id"] == json!(spec_id)), &g["decisions"]);
        if d.len() == 2 {
            let keys = |x: &Value| x["answers"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
            self.r.check("a riding decision answers every question of its spec", keys(&d[0]) == ["verdict", "undo", "risky"], keys(&d[0]));
            self.r.check("ask narrows a riding decision", keys(&d[1]) == ["verdict"], keys(&d[1]));
            let answers: Vec<&Value> = d.iter().flat_map(|x| x["answers"].as_object().into_iter().flat_map(|m| m.values())).collect();
            self.r.check("riding answers are distributions with mass at most 0", answers.iter().all(|a| well_formed(a)), &answers);
            self.r.check("a riding choice names an option", ["allow", "ask", "report"].contains(&d[0]["answers"]["verdict"]["choice"].as_str().unwrap_or("")), &d[0]["answers"]["verdict"]);
        }
        let mut bad = bare.clone();
        bad["decisions"] = json!([{"spec_id": spec_id, "when": "never"}]);
        self.refused("a riding decision with an unknown when is refused", "POST", "/council/v2/generations", Some(bad), 400);
        for (name, ask) in [("an empty ask", json!([])), ("an ask that names a question twice", json!(["verdict", "verdict"]))] {
            let mut b = bare.clone();
            b["decisions"] = json!([{"spec_id": spec_id, "when": "after", "ask": ask}]);
            if let Some(v) = self.refused(&format!("a riding decision with {} is refused", name), "POST", "/council/v2/generations", Some(b), 400) {
                self.r.check(&format!("a riding decision with {} is a 400 naming decisions", name), v["error"]["param"] == "decisions", &v);
            }
        }
        let mut gone = bare;
        gone["decisions"] = json!([{"spec_id": format!("blake3:{}", "0".repeat(64)), "when": "after"}]);
        self.refused("a riding decision on a spec the server does not hold is a 404", "POST", "/council/v2/generations", Some(gone), 404);
    }
}

/// A reply's tool calls as name and input.
fn calls(v: &Value) -> Vec<(Value, Value)> {
    v.as_array().map(|a| a.iter().map(|c| (c["name"].clone(), c["input"].clone())).collect()).unwrap_or_default()
}

/// Probabilities in [0, 1] that sum to 1, and a mass at most 0.
pub(crate) fn well_formed(a: &Value) -> bool {
    let Some(p) = a["probabilities"].as_object() else { return false };
    let ps: Vec<f64> = p.values().filter_map(Value::as_f64).collect();
    ps.len() == p.len() && ps.iter().all(|x| (0.0..=1.0).contains(x)) && (ps.iter().sum::<f64>() - 1.0).abs() < 1e-9 && a["mass"].as_f64().is_some_and(|m| m <= 1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riding_answer_checks_can_fail() {
        assert!(well_formed(&json!({"probabilities": {"a": 0.25, "b": 0.75}, "mass": -0.1})));
        assert!(!well_formed(&json!({"probabilities": {"a": 0.25, "b": 0.5}, "mass": -0.1})));
        assert!(!well_formed(&json!({"probabilities": {"a": 0.25, "b": 0.75}, "mass": 0.2})));
        assert!(!well_formed(&json!({"mass": -0.1})));
    }
}
