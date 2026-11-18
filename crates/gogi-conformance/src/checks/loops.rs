use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::Suite;

/// A loop member.
fn span(start: u32, end: u32, count: u32) -> Value {
    json!({"start": start, "end": end, "count": count})
}

impl Suite {
    /// A context with `loop` set to `member`, unique to this run. `Value::Null` leaves it out.
    fn looped(&self, member: Value) -> Value {
        let mut c = self.ctx(vec![json!({"role": "user", "content": format!("Plan the heist in three steps ({}).", self.nonce), "snap": true})]);
        if !member.is_null() {
            c["loop"] = member;
        }
        c
    }

    /// Checks layer loops on the checked served model: its `layer_loops` member, `count` 1 as the
    /// plain model, model state of its own, `loop: null` beside `parent`, and the refusals in the
    /// contract's order. A served model without `layer_loops` refuses a `loop` naming `loop`.
    pub(crate) fn layer_loops(&mut self) {
        if !self.has(Capability::LayerLoops) {
            let c = self.looped(span(0, 1, 1));
            if let Some(e) = self.refused("a build with a loop to a model without layer_loops is refused", "POST", "/council/v2/contexts", Some(json!({"context": c})), 400) {
                self.r.check("a build with a loop to a model without layer_loops is a 400 naming loop", e["error"]["param"] == json!("loop"), &e);
            }
            self.r.skip("layer loops", "the served model does not declare layer_loops");
            return;
        }
        let Some(me) = self.model.clone() else { return };
        let Some(spans) = me.layer_loops.clone() else {
            self.r.fail("a served model with layer_loops has a layer_loops member", "layer_loops is absent");
            return;
        };
        let b = &spans.bounds;
        let sane = b.len() >= 2 && b.windows(2).all(|w| w[0] < w[1]) && b.last().is_some_and(|l| *l <= spans.layers) && spans.count >= 2;
        self.r.check("layer_loops lists 2 bounds or more in ascending order up to layers, and a count of 2 or more", sane, &spans);
        if !sane {
            return;
        }
        // The shortest span at the first bound, which the most extra runs must allow at count 2.
        let (start, end) = (b[0], b[1]);
        let one = span(start, end, 1);
        let two = span(start, end, 2);
        if u64::from(end - start) > spans.extra_runs {
            self.r.skip("layer loops", "extra_runs does not allow one more run of the shortest span");
            return;
        }

        // count 1 is the plain model, with model state of its own.
        let plain = self.looped(Value::Null);
        match (self.numbers("the plain context reads", &plain), self.numbers("a context with loop count 1 reads", &self.looped(one.clone()))) {
            (Some(p), Some(o)) => {
                self.r.check("a loop with count 1 gives the numbers of no loop", p == o, (&p, &o));
            }
            (None, None) if !self.has(Capability::Reads) && !self.has(Capability::Generate) => self.r.skip("a loop with count 1 gives the numbers of no loop", "the served model has neither reads nor generate"),
            _ => {}
        }
        if let (Some(p), Some(o), Some(t)) = (
            self.build("the plain context builds", &plain, false),
            self.build("a context with loop count 1 builds", &self.looped(one.clone()), false),
            self.build("a context with loop count 2 builds", &self.looped(two.clone()), false),
        ) {
            let ids = |b: &Value| b["snapshots"].as_array().map(|a| a.iter().map(|x| x["id"].clone()).collect::<Vec<_>>()).unwrap_or_default();
            let shared = |b: &Value| ids(b).iter().any(|i| ids(&p).contains(i)) || ids(&p).contains(&b["from"]);
            self.r.check("a loop with count 1 still has model state of its own", o["head"] != p["head"] && !shared(&o), (&o, &p["head"]));
            self.r.check("a loop with count 2 never reuses the plain model state", t["head"] != p["head"] && !shared(&t), (&t, &p["head"]));
            self.r.check("loops of another count never share model state", t["head"] != o["head"], (&t["head"], &o["head"]));
            if let Some(again) = self.build("a context with loop count 2 builds again", &self.looped(two.clone()), false) {
                self.r.check("an equal loop shares its model state", again["head"] == t["head"] && again["usage"]["fed"] == json!(0), &again);
            }
            if t["context"].is_string()
                && let Some(by_parent) = self.build("a build by parent with loop null succeeds", &json!({"parent": t["context"], "loop": null}), false)
            {
                self.r.check("loop: null beside parent drops the parent's loop", by_parent["head"] == p["head"], (&by_parent["head"], &p["head"]));
            }
        }
        if self.has(Capability::Generate)
            && let Some(g) = self.generate("a generation with loop count 2 succeeds", &self.looped(two.clone()), 4)
        {
            self.r.check("a generation with a loop writes a reply", g["usage"]["completion"].as_u64().is_some_and(|n| n > 0), &g["usage"]);
        }

        // The refusals, in the contract's order.
        let outside = (0..=spans.layers + 1).find(|n| !b.contains(n)).unwrap_or(spans.layers + 1);
        let mut refusals: Vec<(String, Value, &str)> = vec![
            ("a loop that is not an object".into(), json!([start, end, 2]), "loop"),
            ("a loop with no count".into(), json!({"start": start, "end": end}), "loop.count"),
            ("a loop with an unknown member".into(), json!({"start": start, "end": end, "count": 2, "mode": "layer"}), "loop.mode"),
            (format!("a start of {} that is not a bound", outside), span(outside, end.max(outside + 1), 2), "loop.start"),
            ("an end equal to start".into(), span(start, start, 2), "loop.end"),
            ("a count of 0".into(), span(start, end, 0), "loop.count"),
            (format!("a count of {}", spans.count + 1), span(start, end, spans.count + 1), "loop.count"),
        ];
        let (first, last) = (b[0], *b.last().expect("2 bounds or more"));
        if u64::from(last - first) * u64::from(spans.count - 1) > spans.extra_runs {
            refusals.push(("more extra runs than extra_runs".into(), span(first, last, spans.count), "loop"));
        } else {
            self.r.skip("a loop past extra_runs is a 400 naming loop", "every span at the most count fits extra_runs");
        }
        for (what, member, param) in refusals {
            let name = format!("a build with {} is a 400 naming {}", what, param);
            if let Some(e) = self.refused(&name, "POST", "/council/v2/contexts", Some(json!({"context": self.looped(member)})), 400) {
                self.r.check(&name, e["error"]["param"] == json!(param), &e);
            }
        }
        if self.has(Capability::Reads) {
            let body = json!({"contexts": [plain.clone(), self.looped(span(start, end, 0))], "suffixes": ["x"], "letters": ["A", "B"]});
            if let Some(e) = self.refused("raw reads with a count of 0 are refused", "POST", "/council/v2/reads", Some(body), 400) {
                self.r.check("raw reads with a count of 0 are a 400 naming contexts[1].loop.count", e["error"]["param"] == json!("contexts[1].loop.count"), &e);
            }
        }
        if self.has(Capability::ControlVectors) {
            let said = |reply: &str| {
                let mut c = self.looped(Value::Null);
                c["messages"].as_array_mut().expect("messages").push(json!({"role": "assistant", "content": reply}));
                c
            };
            let mut with_loop = said("No.");
            with_loop["loop"] = two.clone();
            let body = json!({"positive": [with_loop], "negative": [said("Yes.")]});
            let name = "a control vector request with a looped context is a 400 naming positive[0].loop";
            if let Some((status, v)) = self.send(name, "POST", "/council/v2/control-vectors", Some(body)) {
                self.r.check(name, status == 400 && v["error"]["param"] == json!("positive[0].loop"), (status, &v));
            }
        }
    }
}
