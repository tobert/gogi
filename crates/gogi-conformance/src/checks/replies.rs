use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::{Suite, num};

/// The members of a reply that the same request gives alone.
fn same_reply(reply: &Value, alone: &Value) -> bool {
    ["content", "reasoning", "tool_calls", "finish", "context", "seed"].iter().all(|k| reply[*k] == alone[*k])
}

impl Suite {
    /// Several replies from one generation: `forks` after one context, and one reply after each of `contexts`.
    pub(crate) fn replies(&mut self) {
        if !self.has(Capability::Replies) {
            self.r.skip("several replies", "the server does not declare the replies capability");
            return;
        }
        let limit = self.limits().and_then(|l| l.replies_per_generation);
        self.r.check("a served model with replies names replies_per_generation, at least 1", limit.is_some_and(|n| n >= 1), limit);
        let Some(limit) = limit.filter(|n| *n >= 1) else { return };
        let n = limit.min(3) as usize;
        let msgs = self.opening("replies");
        let ctx = self.ctx(msgs.clone());
        let post = "/council/v2/generations";
        let alone = |seed: u64, temperature: f64, context: &Value| json!({"context": context, "max_tokens": 40, "sample": {"temperature": temperature, "seed": seed}});

        // Forks: each reply is the reply that the same request gives alone with its seed.
        let mut body = alone(7, 1.0, &ctx);
        body["forks"] = json!(n);
        let Some(f) = self.expect("a generation with forks succeeds", "POST", post, Some(body.clone()), 200) else { return };
        let replies = f["replies"].as_array().cloned().unwrap_or_default();
        self.r.check("forks give one reply for each fork, in order", replies.len() == n && replies.iter().enumerate().all(|(i, r)| num(&r["index"]) == i as i64), &f["replies"]);
        self.r.check("the seed of reply i is the seed of the request plus i", replies.iter().enumerate().all(|(i, r)| r["seed"] == json!(7 + i as u64)), replies.iter().map(|r| &r["seed"]).collect::<Vec<_>>());
        self.r.check("a response with replies has no reply of its own", f.get("content").is_none() && f.get("context").is_none() && f["model"] == json!(self.model_name()), &f);
        let mut singles = Vec::new();
        for i in 0..replies.len() {
            if let Some(s) = self.expect("the same request alone with the seed of a fork succeeds", "POST", post, Some(alone(7 + i as u64, 1.0, &ctx)), 200) {
                singles.push(s);
            }
        }
        if singles.len() == replies.len() && !replies.is_empty() {
            self.r.check("a fork is the reply that the same request gives alone", replies.iter().zip(&singles).all(|(r, s)| same_reply(r, s)), (&replies, &singles));
            let completion: i64 = singles.iter().map(|s| num(&s["usage"]["completion"])).sum();
            self.r.check("usage counts the context of forks once and the replies together", f["usage"]["prompt"] == singles[0]["usage"]["prompt"] && num(&f["usage"]["completion"]) == completion && num(&f["usage"]["kept"]) + num(&f["usage"]["fed"]) == num(&f["usage"]["prompt"]), (&f["usage"], completion));
            self.r.check("a fork's usage has its own completion and no prompt", replies.iter().zip(&singles).all(|(r, s)| r["usage"]["completion"] == s["usage"]["completion"] && r["usage"].get("prompt").is_none()), replies.iter().map(|r| &r["usage"]).collect::<Vec<_>>());
        }

        // Ids: the same replies without their text, each read back by its record.
        body["replies"] = json!("ids");
        if let Some(ids) = self.expect("a generation with replies ids succeeds", "POST", post, Some(body.clone()), 200) {
            let short = ids["replies"].as_array().cloned().unwrap_or_default();
            self.r.check("replies ids leave the text out", short.len() == n && short.iter().all(|r| ["content", "reasoning", "tool_calls", "head", "snapshots", "stats"].iter().all(|k| r.get(*k).is_none())), &ids["replies"]);
            self.r.check("replies ids name the records of the same replies", short.iter().zip(&replies).all(|(s, r)| s["context"].is_string() && s["context"] == r["context"] && s["finish"] == r["finish"] && s["seed"] == r["seed"] && s["usage"] == r["usage"]), (&short, &replies));
            if let Some(first) = short.first().and_then(|r| r["context"].as_str())
                && let Some(record) = self.expect("the record of a reply by id reads back", "GET", &format!("/council/v2/contexts/{}", first), None, 200)
            {
                let held = record["context"]["messages"].as_array().cloned().unwrap_or_default();
                self.r.check("a reply reads back later as the last message of its record", held.len() == msgs.len() + 1 && held.last().is_some_and(|m| m["role"] == json!("assistant")), &record);
            }
        }

        // A list of forks gives each reply its own sample.
        let mut listed = alone(7, 1.0, &ctx);
        let entries = [json!({"temperature": 0}), json!({"temperature": 0, "seed": 100})];
        listed["forks"] = json!(entries[..(limit.min(2) as usize)]);
        if let (Some(l), Some(greedy)) = (self.expect("a generation with a list of forks succeeds", "POST", post, Some(listed), 200), self.expect("the same request alone with the sample of a fork succeeds", "POST", post, Some(alone(7, 0.0, &ctx)), 200)) {
            self.r.check("a fork of a list takes the sample of its entry", same_reply(&l["replies"][0], &greedy), (&l["replies"][0], &greedy));
            if limit >= 2 {
                self.r.check("a fork of a list keeps a seed of its own", l["replies"][1]["seed"] == json!(100), &l["replies"][1]["seed"]);
            }
        }
        let mut single = alone(7, 1.0, &ctx);
        single["forks"] = json!(1);
        if let Some(o) = self.expect("a generation with one fork succeeds", "POST", post, Some(single), 200) {
            self.r.check("one fork still has replies", o["replies"].as_array().is_some_and(|r| r.len() == 1) && o.get("content").is_none(), &o);
        }

        // Contexts: one reply after each, and each is the reply of its context alone.
        let gone = format!("blake3:{}", "0".repeat(64));
        if limit >= 2
            && let Some(b) = self.build("build for the contexts of a generation", &ctx, false)
            && let Some(parent) = b["context"].as_str()
        {
            let each: Vec<Value> = ["Start with the tests.", "Start with the docs."].iter().map(|t| json!({"parent": parent, "messages": [{"role": "user", "content": t}]})).collect();
            let mut several = alone(7, 1.0, &each[0]);
            several.as_object_mut().expect("an object").remove("context");
            several["contexts"] = json!(each);
            if let Some(c) = self.expect("a generation with contexts succeeds", "POST", post, Some(several.clone()), 200) {
                let replies = c["replies"].as_array().cloned().unwrap_or_default();
                let singles: Vec<Value> = (0..2).filter_map(|i| self.expect("the same request alone with one of the contexts succeeds", "POST", post, Some(alone(7 + i as u64, 1.0, &each[i])), 200)).collect();
                if singles.len() == 2 {
                    self.r.check("contexts give one reply after each, and each is the reply of its context alone", replies.len() == 2 && replies.iter().zip(&singles).all(|(r, s)| same_reply(r, s)), (&replies, &singles));
                    self.r.check("a reply of contexts has the usage of its own context", replies.iter().zip(&singles).all(|(r, s)| r["usage"]["prompt"] == s["usage"]["prompt"] && r["usage"]["completion"] == s["usage"]["completion"]), replies.iter().map(|r| &r["usage"]).collect::<Vec<_>>());
                    let prompt: i64 = singles.iter().map(|s| num(&s["usage"]["prompt"])).sum();
                    self.r.check("usage counts each context of contexts", num(&c["usage"]["prompt"]) == prompt, (&c["usage"], prompt));
                }
            }
            let mut lost = several.clone();
            lost["contexts"][1] = json!({"parent": gone});
            if let Some(e) = self.refused("contexts with a parent the server does not hold are a 404", "POST", post, Some(lost), 404) {
                self.r.check("an unheld parent of contexts names its context", e["error"]["param"] == json!("contexts[1].parent"), &e);
            }
            let mut bad = several.clone();
            bad["contexts"][1]["messages"][0]["content"] = json!([]);
            if let Some(e) = self.refused("contexts with a bad message are a 400", "POST", post, Some(bad), 400) {
                let want = format!("contexts[1].messages[{}].content", msgs.len());
                self.r.check("a bad message of contexts names its context and its place", e["error"]["param"] == json!(want), (&e, want));
            }
            let mut both = several.clone();
            both["context"] = ctx.clone();
            if let Some(e) = self.refused("context beside contexts is a 400", "POST", post, Some(both), 400) {
                self.r.check("context beside contexts names contexts", e["error"]["param"] == json!("contexts"), &e);
            }
            let mut forked = several;
            forked["forks"] = json!(2);
            if let Some(e) = self.refused("forks beside contexts is a 400", "POST", post, Some(forked), 400) {
                self.r.check("forks beside contexts names forks", e["error"]["param"] == json!("forks"), &e);
            }
        }

        // Refusals.
        for (what, member, value, param) in [
            ("forks of 0 is a 400 naming forks", "forks", json!(0), "forks"),
            ("forks past replies_per_generation is a 400 naming forks", "forks", json!(limit + 1), "forks"),
            ("forks that is not a count or a list is a 400 naming forks", "forks", json!("two"), "forks"),
            ("replies with no forks is a 400 naming replies", "replies", json!("ids"), "replies"),
        ] {
            let mut b = alone(7, 1.0, &ctx);
            b[member] = value;
            if let Some(e) = self.refused(what, "POST", post, Some(b), 400) {
                self.r.check(what, e["error"]["param"] == json!(param), &e);
            }
        }
        for (what, member, value) in [("a stream with forks is a 400 naming stream", "stream", json!(true)), ("replies that is not full or ids is a 400 naming replies", "replies", json!("text"))] {
            let mut b = alone(7, 1.0, &ctx);
            b["forks"] = json!(1);
            b[member] = value;
            if let Some(e) = self.refused(what, "POST", post, Some(b), 400) {
                self.r.check(what, e["error"]["param"] == json!(member), &e);
            }
        }
    }
}
