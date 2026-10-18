use gogi_proto::identity::Capability;
use gogi_proto::reads::ReadsResponse;
use serde_json::{Value, json};

use crate::{Suite, num};

const SUFFIX: &str = "\nAnswer: ";

impl Suite {
    pub(crate) fn reads(&mut self) {
        if !self.has(Capability::Reads) {
            self.r.skip("reads", "the server does not declare the reads capability");
            return;
        }
        let c0 = self.ctx(self.opening("read"));
        let c1 = self.ctx(vec![json!({"role": "system", "content": format!("Another reader ({}).", self.nonce)}), json!({"role": "user", "content": "Be brief.", "snap": true})]);
        let Some(b0) = self.build("build before reads", &c0, false) else { return };
        let b1 = self.build("second build before reads", &c1, false);
        let req = json!({"contexts": [c0, c1], "suffixes": [SUFFIX, "\nVerdict: "], "letters": ["A", "B", "C"], "top": 4});
        let Some(r) = self.expect("reads succeed", "POST", "/council/v2/reads", Some(req.clone()), 200) else { return };
        let Some(resp) = self.decode::<ReadsResponse>("reads response has the contract's fields", &r) else { return };
        let order: Vec<(u64, u64)> = resp.reads.iter().map(|x| (x.context, x.suffix)).collect();
        self.r.check("reads come context by context, suffix by suffix", order == [(0, 0), (0, 1), (1, 0), (1, 1)], order);
        self.r.check("each read names the snapshot it read after", resp.reads[0].snapshot.as_ref().map(|s| s.as_str()) == b0["head"].as_str() && b1.as_ref().is_none_or(|b| resp.reads[2].snapshot.as_ref().map(|s| s.as_str()) == b["head"].as_str()), &r["reads"]);
        self.r.check("logprobs follow the letters in order", resp.reads.iter().all(|x| x.logprobs.keys().collect::<Vec<_>>() == ["A", "B", "C"]), &r["reads"][0]["logprobs"]);
        let mass_ok = resp.reads.iter().all(|x| {
            let lp: Vec<f64> = x.logprobs.values().copied().collect();
            x.logprobs.values().all(|v| *v <= 0.0) && (x.mass - gogi_proto::math::log_sum_exp(&lp)).abs() <= 1e-9
        });
        self.r.check("mass is the log of the letters' summed probability", mass_ok, &r["reads"][0]);
        self.r.check("top lists the requested count, most likely first", resp.reads.iter().all(|x| x.top.len() == 4 && x.top.windows(2).all(|w| w[0].logprob >= w[1].logprob) && x.top.iter().all(|t| t.logprob <= 0.0)), &r["reads"][0]["top"]);
        self.r.check("each top token has an id", resp.reads.iter().all(|x| x.top.iter().all(|t| t.id.is_some())), &r["reads"][0]["top"]);
        self.r.check("a letter's logprob is no more than the top token's", resp.reads.iter().all(|x| x.logprobs.values().all(|v| *v <= x.top[0].logprob + 1e-12)), &r["reads"][0]);
        self.r.check("reads report signals", r["signals"]["control_text"].is_array() || r.get("signals").is_none(), &r["signals"]);

        // A single context and suffix reads the same as the same pair inside a larger request.
        let single = json!({"contexts": [req["contexts"][1].clone()], "suffixes": ["\nVerdict: "], "letters": ["A", "B", "C"], "top": 4});
        if let Some(s) = self.expect("a single read succeeds", "POST", "/council/v2/reads", Some(single), 200) {
            self.r.check("a read does not depend on its neighbors", s["reads"][0]["logprobs"] == r["reads"][3]["logprobs"] && s["reads"][0]["top"] == r["reads"][3]["top"], (&s["reads"][0], &r["reads"][3]));
        }

        let bad: Vec<(&str, Value)> = vec![
            ("reads with no contexts", json!({"contexts": [], "suffixes": ["x"], "letters": ["A", "B"]})),
            ("reads with no suffix", json!({"contexts": [req["contexts"][0].clone()], "suffixes": [], "letters": ["A", "B"]})),
            ("reads with one letter", json!({"contexts": [req["contexts"][0].clone()], "suffixes": ["x"], "letters": ["A"]})),
            ("reads with a repeated letter", json!({"contexts": [req["contexts"][0].clone()], "suffixes": ["x"], "letters": ["A", "A"]})),
            ("reads with nine contexts", json!({"contexts": vec![req["contexts"][0].clone(); 9], "suffixes": ["x"], "letters": ["A", "B"]})),
            ("reads on a bad context", json!({"contexts": [self.ctx(vec![json!({"role": "tool", "tool_call_id": "z", "content": "x"})])], "suffixes": ["x"], "letters": ["A", "B"]})),
            ("reads with an unknown member", json!({"contexts": [req["contexts"][0].clone()], "suffixes": ["x"], "letters": ["A", "B"], "extra": 1})),
        ];
        for (name, body) in bad {
            self.refused(&format!("{} are refused", name), "POST", "/council/v2/reads", Some(body), 400);
        }
        let limit = self.identity.as_ref().map_or(0, |i| i.limits.context_tokens) as usize;
        if limit * 7 <= 900_000 {
            let big = self.ctx(vec![json!({"role": "user", "content": super::generations::filler_text(limit * 6, &self.nonce)})]);
            self.refused("reads past context_tokens are refused", "POST", "/council/v2/reads", Some(json!({"contexts": [big], "suffixes": ["x"], "letters": ["A", "B"]})), 413);
        }
    }

    /// Raw reads replay bit for bit, whatever else the server ran between them.
    pub(crate) fn replay(&mut self) {
        if !self.has(Capability::Reads) || !self.has(Capability::Generate) {
            self.r.skip("reads replay across other traffic", "needs the reads and generate capabilities");
            return;
        }
        let msgs = self.opening("replay");
        let Some(g) = self.generate("replay: the first generation succeeds", &self.ctx(msgs.clone()), 300) else { return };
        let mut m2 = msgs;
        m2.extend(self.continue_after(&g, "Now list what is left."));
        let rq = json!({"contexts": [self.ctx(m2)], "suffixes": [SUFFIX], "letters": ["A", "B"], "top": 20});
        let Some(a) = self.expect("replay: the first read succeeds", "POST", "/council/v2/reads", Some(rq.clone()), 200) else { return };
        let other = json!({"messages": [{"role": "system", "content": format!("Unrelated {}.", self.nonce)}, {"role": "user", "content": "Say hi.", "snap": true}]});
        self.generate("replay: an unrelated generation succeeds", &other, 8);
        let Some(b) = self.expect("replay: the second read succeeds", "POST", "/council/v2/reads", Some(rq), 200) else { return };
        let same = a["reads"][0]["top"] == b["reads"][0]["top"] && a["reads"][0]["logprobs"] == b["reads"][0]["logprobs"] && a["reads"][0]["mass"] == b["reads"][0]["mass"];
        self.r.check("a read replays bit for bit after an unrelated generation", same, (&a["reads"][0]["logprobs"], &b["reads"][0]["logprobs"]));
        self.r.check("the replayed read names the same snapshot", a["reads"][0]["snapshot"] == b["reads"][0]["snapshot"] && num(&a["reads"][0]["context"]) == 0, &a["reads"][0]["snapshot"]);
    }
}
