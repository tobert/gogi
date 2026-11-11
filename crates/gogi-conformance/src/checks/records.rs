use gogi_proto::generation::{Finish, GenerationDone, Phase};
use gogi_proto::identity::Capability;
use gogi_proto::{Context, context_id};
use serde_json::{Value, json};

use super::continue_after;
use crate::{Suite, num};

impl Suite {
    /// Context records: a response names the record of its context, and a request names a record as
    /// its `parent` and sends only the messages after it.
    pub(crate) fn records(&mut self) {
        let msgs = self.opening("record");
        let ctx = self.ctx(msgs.clone());
        let Some(b) = self.build("build for records", &ctx, false) else { return };
        let Some(id) = b["context"].as_str().map(String::from) else {
            self.r.fail("a build names the record of its context", format!("the build has no context: {}", b));
            return;
        };
        self.r.check("a build names the record of its context", gogi_proto::content_id_problem(&id).is_none(), &b["context"]);
        let own: Result<Context, _> = serde_json::from_value(ctx.clone());
        self.r.check("the context id is the content id of the record's text", own.as_ref().is_ok_and(|c| context_id(c).as_str() == id), (&id, own.as_ref().map(|c| context_id(c).to_string()).ok()));
        let l = self.identity.as_ref().and_then(|i| i.limits.context_records);
        self.r.check("identity names context_records, at least 1", l.is_some_and(|n| n >= 1), l);

        // The record reads back as its context.
        let path = format!("/council/v2/contexts/{}", id);
        if let Some(r) = self.expect("a record reads back", "GET", &path, None, 200) {
            self.r.check("a record reads back with its id and no parent", r["id"] == json!(id) && r["context"].get("parent").is_none(), &r);
            self.r.check("a record reads back as its context", serde_json::from_value::<Context>(r["context"].clone()).ok() == own.as_ref().ok().cloned(), (&r["context"], &ctx));
        }
        let gone = format!("blake3:{}", "0".repeat(64));
        self.refused("an unknown record is a 404", "GET", &format!("/council/v2/contexts/{}", gone), None, 404);
        if let Some(e) = self.refused("a record id that is not whole is a 400", "GET", "/council/v2/contexts/blake3:ab", None, 400) {
            self.r.check("a record id that is not whole names id", e["error"]["param"] == json!("id"), &e);
        }

        // A parent alone is the same context.
        let alone = json!({"parent": id});
        if let Some(p) = self.build("a build of a parent alone succeeds", &alone, false) {
            self.r.check("a parent alone is the same context", p["context"] == b["context"] && p["head"] == b["head"] && p["usage"]["prompt"] == b["usage"]["prompt"] && num(&p["usage"]["fed"]) == 0, (&p, &b));
        }

        // A parent and a new message are the whole context with that message.
        let more = json!({"role": "user", "content": "Then run the tests.", "snap": true});
        let by_parent = json!({"parent": id, "messages": [more.clone()]});
        let mut longer = msgs.clone();
        longer.push(more);
        let whole = self.ctx(longer);
        if let Some(p) = self.build("a build of a parent and a new message succeeds", &by_parent, false) {
            self.r.check("a build from a parent starts from the parent's snapshots", num(&p["usage"]["kept"]) >= num(&b["usage"]["prompt"]), (&p["usage"], &b["usage"]));
            if let Some(w) = self.build("the same context sent whole builds", &whole, false) {
                self.r.check("a context sent with a parent and sent whole is one context", p["context"] == w["context"] && p["head"] == w["head"] && p["usage"]["prompt"] == w["usage"]["prompt"] && num(&w["usage"]["fed"]) == 0, (&p, &w));
            }
        }

        // A member beside parent replaces the parent's.
        let bare = json!({"parent": id, "tools": []});
        if let Some(p) = self.build("a build with a member beside parent succeeds", &bare, false)
            && let Some(r) = self.expect("the record with the member replaced reads back", "GET", &format!("/council/v2/contexts/{}", p["context"].as_str().unwrap_or("")), None, 200)
        {
            self.r.check("a member beside parent replaces the parent's", p["context"] != b["context"] && r["context"].get("tools").is_none() && r["context"]["messages"] == json!(msgs) && r["context"]["thinking"] == ctx["thinking"], &r);
        }

        // A dry run holds no record.
        let dry = self.ctx(self.opening("record-dry"));
        if let Some(d) = self.build("a dry run for records succeeds", &dry, true) {
            self.r.check("a dry run names no record", d.get("context").is_none(), &d);
            if let Ok(c) = serde_json::from_value::<Context>(dry.clone()) {
                self.refused("a dry run holds no record", "GET", &format!("/council/v2/contexts/{}", context_id(&c)), None, 404);
            }
        }

        // Refusals.
        let lost = json!({"parent": gone, "messages": [{"role": "user", "content": "more"}]});
        if let Some(e) = self.refused("a parent the server does not hold is a 404", "POST", "/council/v2/contexts", Some(json!({"context": lost})), 404) {
            self.r.check("a parent the server does not hold names context.parent and quotes the id", e["error"]["type"] == json!("not_found") && e["error"]["param"] == json!("context.parent") && e["error"]["message"].as_str().is_some_and(|m| m.contains(&gone)), &e);
        }
        for (what, context, param) in [
            ("a parent that is not a whole content id is a 400 naming context.parent", json!({"parent": "blake3:ab"}), "context.parent"),
            ("a parent that is not text is a 400 naming context.parent", json!({"parent": 7}), "context.parent"),
            ("a member that does not stand beside parent is a 400 naming it", json!({"parent": id, "dry_run": true}), "context.dry_run"),
            ("messages beside parent that are not a list are a 400 naming context.messages", json!({"parent": id, "messages": "more"}), "context.messages"),
        ] {
            if let Some(e) = self.refused(what, "POST", "/council/v2/contexts", Some(json!({"context": context})), 400) {
                self.r.check(what, e["error"]["param"] == json!(param), &e);
            }
        }
        // A new message is checked as a message of the whole context, so its error names its place there.
        let bad = json!({"parent": id, "messages": [{"role": "user", "content": []}]});
        if let Some(e) = self.refused("a bad new message is a 400", "POST", "/council/v2/contexts", Some(json!({"context": bad})), 400) {
            let want = format!("messages[{}].content", msgs.len());
            self.r.check("a bad new message names its place in the whole context", e["error"]["param"] == json!(want), (&e, want));
        }
        let spec = self.gate_spec("record");
        if let Some(held) = self.expect("spec post for records succeeds", "POST", "/council/v2/specs", Some(spec), 200) {
            let req = json!({"spec_id": held["spec_id"], "case": "git push origin main", "contexts": [{"parent": id}, {"parent": gone}]});
            if let Some(e) = self.refused("a decision with a parent the server does not hold is a 404", "POST", "/council/v2/decisions", Some(req), 404) {
                self.r.check("a decision names the parent under contexts", e["error"]["param"] == json!("contexts[1].parent"), &e);
            }
            let req = json!({"spec_id": held["spec_id"], "case": "git push origin main", "contexts": [{"parent": id}]});
            let whole = json!({"spec_id": held["spec_id"], "case": "git push origin main", "contexts": [ctx.clone()]});
            if let (Some(p), Some(w)) = (self.expect("a decision reads a parent", "POST", "/council/v2/decisions", Some(req), 200), self.expect("the same decision with the context whole succeeds", "POST", "/council/v2/decisions", Some(whole), 200)) {
                self.r.check("a decision from a parent gives the numbers of the whole context", p["answers"] == w["answers"] && p["reads"] == w["reads"], (&p["answers"], &w["answers"]));
            }

            // A decision holds the record of each context it reads.
            let fresh = self.ctx(self.opening("record-decision"));
            let req = json!({"spec_id": held["spec_id"], "case": "git push origin main", "contexts": [fresh.clone()]});
            if self.expect("a decision for a record succeeds", "POST", "/council/v2/decisions", Some(req), 200).is_some()
                && let Ok(c) = serde_json::from_value::<Context>(fresh)
                && let Some(r) = self.expect("the record of a decision's context reads back", "GET", &format!("/council/v2/contexts/{}", context_id(&c)), None, 200)
            {
                self.r.check("a decision holds the record of its context", r["id"] == json!(context_id(&c).as_str()), &r);
            }
        }

        self.record_pins(&id, &gone);
        self.record_replies(&ctx, &msgs);
    }

    fn record_pins(&mut self, id: &str, gone: &str) {
        if !self.has(Capability::Pins) {
            self.r.skip("a pin keeps a context record", "the server does not declare the pins capability");
            return;
        }
        let path = format!("/council/v2/pins/gogi-conformance-records-{}", self.nonce);
        if let Some(p) = self.expect("a pin of records succeeds", "PUT", &path, Some(json!({"snapshots": [], "contexts": [id, gone], "ttl_s": 60})), 200) {
            self.r.check("a pin reports each record with whether it is held", p["contexts"] == json!([{"id": id, "held": true}, {"id": gone, "held": false}]) && p["expires_in_s"] == json!(60), &p);
        }
        if let Some(g) = self.expect("a pin list with records succeeds", "GET", &path, None, 200) {
            self.r.check("a pin list names the held records only", g["contexts"] == json!([id]), &g);
        }
        if let Some(e) = self.refused("a pin of a record id that is not whole is a 400", "PUT", &path, Some(json!({"snapshots": [], "contexts": ["blake3:ab"], "ttl_s": 60})), 400) {
            self.r.check("a pin of a record id that is not whole names contexts", e["error"]["param"] == json!("contexts"), &e);
        }
        self.expect("a pin of records deletes", "DELETE", &path, None, 204);
    }

    /// The reply in a record: a generation's `context` is the request's context with the reply after it.
    fn record_replies(&mut self, ctx: &Value, msgs: &[Value]) {
        if !self.has(Capability::Generate) {
            self.r.skip("a generation names the record of its context and its reply", "the server does not declare the generate capability");
            return;
        }
        let Some(g) = self.generate("generation for records succeeds", ctx, 300) else { return };
        let Some(done) = self.decode::<GenerationDone>("generation for records has the contract's fields", &g) else { return };
        let Some(rid) = done.context.as_ref().map(|c| c.to_string()) else {
            self.r.fail("a generation names the record of its context and its reply", format!("done has no context: {}", g));
            return;
        };
        let Some(r) = self.expect("the record of a reply reads back", "GET", &format!("/council/v2/contexts/{}", rid), None, 200) else { return };
        let held = r["context"]["messages"].as_array().cloned().unwrap_or_default();
        let reply = held.last().cloned().unwrap_or(Value::Null);
        self.r.check("a generation names the record of its context and its reply", held.len() == msgs.len() + 1 && held[..msgs.len()] == *msgs && reply["role"] == json!("assistant"), &r);
        if done.finish != Finish::MaxTokens {
            // The record's reply is the reply as a client sends it back, with a mark.
            let mut sent = continue_after(&g, "unused")[0].clone();
            sent["snap"] = json!(true);
            self.r.check("the reply in a record is the reply as a client sends it back, marked", reply == sent, (&reply, &sent));
            if g["head"].is_string()
                && let Some(again) = self.build("the record of a reply builds", &json!({"parent": rid}), false)
            {
                self.r.check("the record of a reply builds to the head of the generation", again["head"] == g["head"] && num(&again["usage"]["fed"]) == 0, (&again, &g["head"]));
            }
            // The next request names the record and sends only what is new.
            let rest: Vec<Value> = continue_after(&g, "Now list what is left.")[1..].to_vec();
            let by_parent = json!({"parent": rid, "messages": rest});
            let mut all = held.clone();
            all.extend(rest);
            let mut whole = ctx.clone();
            whole["messages"] = json!(all);
            if let (Some(p), Some(w)) = (self.generate("a generation from a parent succeeds", &by_parent, 100), self.generate("the same generation with the context whole succeeds", &whole, 100)) {
                self.r.check("a generation from a parent is the generation from the whole context", p["content"] == w["content"] && p["reasoning"] == w["reasoning"] && p["tool_calls"] == w["tool_calls"] && p["context"] == w["context"] && p["usage"]["prompt"] == w["usage"]["prompt"], (&p, &w));
            }
        }

        // A reply cut by max_tokens is an open message in its record, and the record alone continues it.
        if !self.has(Capability::Continuation) {
            self.r.skip("a cut reply is an open message in its record", "the server does not declare the continuation capability");
            return;
        }
        let Some(c) = self.generate("a cut generation for records succeeds", ctx, 1) else { return };
        let Some(cut) = self.decode::<GenerationDone>("a cut generation for records has the contract's fields", &c) else { return };
        let (Some(cid), true) = (cut.context.as_ref().map(|c| c.to_string()), cut.finish == Finish::MaxTokens && matches!(cut.phase, Some(Phase::Content | Phase::Reasoning)) && cut.as_open_message().is_some()) else {
            self.r.skip("a cut reply is an open message in its record", "one token did not cut the reply in its content or its thinking");
            return;
        };
        if let Some(r) = self.expect("the record of a cut reply reads back", "GET", &format!("/council/v2/contexts/{}", cid), None, 200) {
            let last = r["context"]["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or(Value::Null);
            self.r.check("a cut reply is an open message in its record", last["continue"] == json!(true) && last.get("content").is_some() == (cut.phase == Some(Phase::Content)), &last);
        }
        if let Some(n) = self.generate("a generation from the record of a cut reply continues it", &json!({"parent": cid}), 4) {
            self.r.check("a continuation from a record starts with the cut reply", n["content"].as_str().is_some_and(|t| t.starts_with(&cut.content)) && n["reasoning"].as_str().is_some_and(|t| t.starts_with(&cut.reasoning)), (&n, &c));
        }
        if let Some(closed) = self.build("a build of the record of a cut reply succeeds", &json!({"parent": cid}), false) {
            // The closed context is another context than the open one, so it has another record.
            self.r.check("a build reads the record of a cut reply closed", closed["context"].is_string() && closed["context"] != json!(cid), &closed);
        }
    }
}
