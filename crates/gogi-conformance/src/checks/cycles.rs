use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    /// A context with `cycles` set to `member`, unique to this run. `Value::Null` leaves it out.
    fn counted(&self, member: Value) -> Value {
        let mut c = self.ctx(vec![json!({"role": "user", "content": format!("Plan the heist in three steps, with cycles ({}).", self.nonce), "snap": true})]);
        if !member.is_null() {
            c["cycles"] = member;
        }
        c
    }

    /// Checks cycles on the checked served model: its `cycles` member, the default count as the
    /// model's own, model state of its own for each other count, `cycles: null` beside `parent`,
    /// and the refusals in the contract's order. A served model without `cycles` refuses a
    /// `cycles` naming `cycles`.
    pub(crate) fn cycles(&mut self) {
        if !self.has(Capability::Cycles) {
            let c = self.counted(json!(1));
            self.refused_naming("a build with cycles to a model without cycles is a 400 naming cycles", "POST", "/council/v2/contexts", Some(json!({"context": c})), "cycles", "does not declare cycles");
            let c = self.counted(json!(0));
            self.refused_naming("a count of 0 to a model without cycles is refused for the capability first", "POST", "/council/v2/contexts", Some(json!({"context": c})), "cycles", "does not declare cycles");
            self.r.skip("cycles", "the served model does not declare cycles");
            return;
        }
        let Some(me) = self.model.clone() else { return };
        let Some(range) = me.cycles else {
            self.r.fail("a served model with cycles has a cycles member", "cycles is absent");
            return;
        };
        self.r.check("cycles has 1 <= min <= default <= max and a span with start below end", range.is_sane(), range);
        if !range.is_sane() {
            return;
        }
        let default = json!(range.default);
        let plain = self.counted(Value::Null);
        let named = self.counted(default.clone());

        // Another count, when the server runs one.
        let other = [range.default + 1, range.default.saturating_sub(1)].into_iter().find(|n| *n >= range.min && *n <= range.max && *n != range.default);
        let Some(count) = other else {
            self.r.skip("a count other than the default", "the server runs one count only");
            self.cycles_refusals(&range, &plain);
            return;
        };
        let counted = self.counted(json!(count));
        // Builds first: a read holds snapshots, and the checks of sharing start from none.
        if let (Some(p), Some(n)) = (self.build("the plain context builds", &plain, false), self.build("a context with cycles at the default builds", &named, false)) {
            self.r.check("cycles at the default shares the model state of no cycles", n["head"] == p["head"] && n["usage"]["fed"] == json!(0) && n["usage"]["kept"] == n["usage"]["prompt"], (&n["head"], &p["head"], &n["usage"]));
            self.r.check("a context with cycles and one without are two records", n["context"] != p["context"], (&n["context"], &p["context"]));
            if let Some(o) = self.build("a context at another count builds", &counted, false) {
                self.r.check("another count never shares model state with the default", o["head"] != p["head"] && o["usage"]["kept"] == json!(0) && o["from"].is_null(), (&o, &p["head"]));
                if let Some(again) = self.build("a context at another count builds again", &counted, false) {
                    self.r.check("an equal count shares its model state", again["head"] == o["head"] && again["usage"]["fed"] == json!(0), &again);
                }
                if o["context"].is_string()
                    && let Some(by_parent) = self.build("a build by parent with cycles null succeeds", &json!({"parent": o["context"], "cycles": null}), false)
                {
                    self.r.check("cycles: null beside parent drops the parent's count", by_parent["head"] == p["head"], (&by_parent["head"], &p["head"]));
                }
            }
        }
        // Then the numbers.
        // The default count is the model's own count: the numbers and the model state of no cycles.
        match (self.numbers("the plain context reads", &plain), self.numbers("a context with cycles at the default reads", &named)) {
            (Some(p), Some(n)) => {
                self.r.check("cycles at the default gives the numbers of no cycles", p == n, (&p, &n));
            }
            (None, None) if !self.has(Capability::Reads) && !self.has(Capability::Generate) => self.r.skip("cycles at the default gives the numbers of no cycles", "the served model has neither reads nor generate"),
            _ => {}
        }
        if let (Some(p), Some(o)) = (self.numbers("the plain context reads again", &plain), self.numbers("a context at another count reads", &counted)) {
            self.r.check("a context at another count gives other numbers", p != o, (&p, &o));
        }
        if self.has(Capability::Generate)
            && let Some(g) = self.generate("a generation at another count succeeds", &counted, 4)
        {
            self.r.check("a generation at another count writes a reply", g["usage"]["completion"].as_u64().is_some_and(|n| n > 0), &g["usage"]);
        }
        self.cycles_refusals(&range, &plain);
    }

    /// The refusals of cycles, in the contract's order, and the places that name `cycles`.
    fn cycles_refusals(&mut self, range: &gogi_proto::cycles::Cycles, plain: &Value) {
        let refusals: Vec<(String, Value, &str)> = vec![
            ("cycles that is not a number".into(), json!("3"), "whole number"),
            ("cycles that is not a whole number".into(), json!(2.5), "whole number"),
            ("cycles below 0".into(), json!(-1), "whole number"),
            ("cycles of null".into(), Value::Null, "whole number"),
            // The least count is 1 or more, so the count below it is 0 or more.
            (format!("a count of {}, below the least", range.min - 1), json!(range.min - 1), "cycles"),
            (format!("a count of {}, above the most", range.max + 1), json!(range.max + 1), "cycles"),
        ];
        for (what, member, named) in refusals {
            let name = format!("a build with {} is a 400 naming cycles", what);
            let mut c = plain.clone();
            c["cycles"] = member.clone();
            let named = if named == "cycles" { format!("{} to {}", range.min, range.max) } else { named.to_string() };
            self.refused_naming(&name, "POST", "/council/v2/contexts", Some(json!({"context": c})), "cycles", &named);
        }
        if self.has(Capability::Reads) {
            let mut bad = plain.clone();
            bad["cycles"] = json!(range.max + 1);
            let body = json!({"contexts": [plain.clone(), bad], "suffixes": ["x"], "letters": ["A", "B"]});
            if let Some(e) = self.refused("raw reads with a count above the most are refused", "POST", "/council/v2/reads", Some(body), 400) {
                self.r.check("raw reads with a count above the most are a 400 naming contexts[1].cycles", e["error"]["param"] == json!("contexts[1].cycles"), &e);
            }
        }
        if self.has(Capability::ControlVectors) {
            let said = |s: &Suite, reply: &str, cycles: Value| {
                let mut c = s.counted(Value::Null);
                c["messages"].as_array_mut().expect("messages").push(json!({"role": "assistant", "content": reply}));
                if !cycles.is_null() {
                    c["cycles"] = cycles;
                }
                c
            };
            // A count equal to the default is refused too: a control vector sends no cycles.
            let body = json!({"positive": [said(self, "No.", json!(range.default))], "negative": [said(self, "Yes.", Value::Null)]});
            let name = "a control vector request with cycles is a 400 naming positive[0].cycles";
            if let Some((status, v)) = self.send(name, "POST", "/council/v2/control-vectors", Some(body)) {
                self.r.check(name, status == 400 && v["error"]["param"] == json!("positive[0].cycles"), (status, &v));
            }
        }
        // A loop and cycles together: each member's own check comes first.
        if self.has(Capability::LayerLoops)
            && let Some(spans) = self.model.as_ref().and_then(|m| m.layer_loops.clone())
            && spans.bounds.len() >= 2
        {
            let (start, end) = (spans.bounds[0], spans.bounds[1]);
            let with_count = |loop_member: Value, cycles: u32| {
                let mut c = plain.clone();
                c["loop"] = loop_member;
                c["cycles"] = json!(cycles);
                c
            };
            let with = |loop_member: Value| with_count(loop_member, range.default);
            let name = "a build with a loop and cycles is a 400 naming cycles";
            self.refused_naming(name, "POST", "/council/v2/contexts", Some(json!({"context": with(json!({"start": start, "end": end, "count": 1}))})), "cycles", "loop and cycles");
            let name = "a build with a loop and a count out of range is a 400 naming cycles, with the range, the member's own check first";
            let body = json!({"context": with_count(json!({"start": start, "end": end, "count": 1}), range.max + 1)});
            self.refused_naming(name, "POST", "/council/v2/contexts", Some(body), "cycles", &format!("{} to {}", range.min, range.max));
            let name = "a build with a bad loop and cycles is a 400 naming loop.count, the member's own check first";
            self.refused_naming(name, "POST", "/council/v2/contexts", Some(json!({"context": with(json!({"start": start, "end": end, "count": 0}))})), "loop.count", "0");
        } else {
            self.r.skip("a loop and cycles together", "the served model does not declare layer_loops");
        }
    }
}
