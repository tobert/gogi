//! Several replies from one generation request: `forks` after one context, or one reply after
//! each of `contexts`.
//!
//! `split` turns such a request into the requests of its replies, each a plain generation. A
//! server runs each one as it runs any generation and joins the results with `join`. Thus reply
//! `i` is the reply that the same request gives alone, and no rule of a generation has a second
//! form.

use serde_json::{Map, Value, json};

use crate::error::Invalid;
use crate::generation::GenerationDone;

/// What each reply of the response holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// Every member of `done` but `model`.
    Full,
    /// The record of the reply and its counts, without its text.
    Ids,
}

/// A request for several replies, as the requests of its replies.
#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    /// One plain generation body for each reply, in order.
    pub requests: Vec<Value>,
    pub wanted: Wanted,
    /// `true` for `forks`: each request reads the same context.
    pub forks: bool,
}

fn invalid(param: &str, message: impl Into<String>) -> Invalid {
    Invalid::param(param, format!("nothing ran: {}", message.into()))
}

/// The requests of the replies of `body`, or `None` for a body with neither `forks` nor
/// `contexts`: a plain generation. `limit` is the served model's `replies_per_generation`.
/// `first_seed` gives the seed of reply 0 when the request has none.
pub fn split(body: &Value, limit: u64, first_seed: impl FnOnce() -> u64) -> Result<Option<Split>, Invalid> {
    let Some(members) = body.as_object() else { return Ok(None) };
    let (forks, contexts) = (members.get("forks"), members.get("contexts"));
    if forks.is_none() && contexts.is_none() {
        return match members.get("replies") {
            Some(_) => Err(invalid("replies", "replies stands only beside forks or contexts")),
            None => Ok(None),
        };
    }
    if forks.is_some() && contexts.is_some() {
        return Err(invalid("forks", "forks stands only beside context; a request with contexts writes one reply after each"));
    }
    if contexts.is_some() && members.contains_key("context") {
        return Err(invalid("contexts", "a request has context or contexts, not both"));
    }
    if forks.is_some() && !members.contains_key("context") {
        return Err(invalid("context", "missing field `context`: forks writes several replies after one context"));
    }
    for (name, off) in [("stream", json!(false)), ("output", Value::Null), ("decisions", json!([])), ("previews", json!(false))] {
        if members.get(name).is_some_and(|v| *v != off && !v.is_null()) {
            return Err(invalid(name, format!("a request with several replies takes no {}", name)));
        }
    }
    let wanted = match members.get("replies") {
        None => Wanted::Full,
        Some(v) if v == "full" => Wanted::Full,
        Some(v) if v == "ids" => Wanted::Ids,
        Some(other) => return Err(invalid("replies", format!("replies is \"full\" or \"ids\", not {}", other))),
    };
    // The context and the sample of each reply.
    let within = |n: usize, name: &str| if n == 0 || n as u64 > limit { Err(invalid(name, format!("{} asks for {} replies; a generation writes 1 to {} (replies_per_generation)", name, n, limit))) } else { Ok(()) };
    let mut each: Vec<(Value, Map<String, Value>)> = Vec::new();
    if let Some(list) = contexts {
        let Some(list) = list.as_array() else { return Err(invalid("contexts", format!("contexts is a list of contexts, not {}", list))) };
        within(list.len(), "contexts")?;
        each.extend(list.iter().map(|c| (c.clone(), Map::new())));
    } else {
        let context = members["context"].clone();
        match forks.expect("forks or contexts") {
            Value::Number(n) if n.as_u64().is_some() => {
                let n = n.as_u64().expect("checked") as usize;
                within(n, "forks")?;
                each.extend((0..n).map(|_| (context.clone(), Map::new())));
            }
            Value::Array(list) => {
                within(list.len(), "forks")?;
                for (i, entry) in list.iter().enumerate() {
                    let Some(entry) = entry.as_object() else { return Err(invalid("forks", format!("forks[{}] is a sample object, not {}", i, entry))) };
                    each.push((context.clone(), entry.clone()));
                }
            }
            other => return Err(invalid("forks", format!("forks is a count or a list of sample objects, not {}", other))),
        }
    }
    let sample = match members.get("sample") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(s)) => s.clone(),
        Some(other) => return Err(invalid("sample", format!("sample is an object, not {}", other))),
    };
    let first = match sample.get("seed").and_then(Value::as_u64) {
        Some(seed) => seed,
        None => first_seed(),
    };
    let mut base = members.clone();
    for name in ["forks", "contexts", "replies", "context"] {
        base.remove(name);
    }
    let mut requests = Vec::new();
    for (i, (context, own)) in each.into_iter().enumerate() {
        let mut sample = sample.clone();
        // The seed of reply `i` is the first seed plus `i`, unless its entry gives one.
        sample.insert("seed".into(), json!(first.wrapping_add(i as u64)));
        sample.extend(own);
        let mut request = base.clone();
        request.insert("context".into(), context);
        request.insert("sample".into(), Value::Object(sample));
        requests.push(Value::Object(request));
    }
    Ok(Some(Split { requests, wanted, forks: forks.is_some() }))
}

/// The name of an error's `param` for reply `i` of a request with `contexts`: a member of the
/// context goes under `contexts[i]`. Another member, such as `max_tokens`, keeps its name.
pub fn param_in(param: &str, i: usize) -> String {
    match param.strip_prefix("context.") {
        Some(rest) => format!("contexts[{}].{}", i, rest),
        None if ["messages", "thinking", "tools", "queue", "control", "continue"].iter().any(|m| param == *m || param.starts_with(&format!("{}[", m)) || param.starts_with(&format!("{}.", m))) => format!("contexts[{}].{}", i, param),
        None => param.to_string(),
    }
}

/// The response of a request with several replies, from the `done` of each reply in order.
pub fn join(split: &Split, dones: &[GenerationDone]) -> Value {
    let mut usage = json!({"prompt": 0u64, "kept": 0u64, "fed": 0u64, "completion": 0u64, "reasoning": 0u64});
    let add = |total: &mut Value, name: &str, n: u64| total[name] = json!(total[name].as_u64().unwrap_or(0) + n);
    let mut replies = Vec::new();
    for (i, done) in dones.iter().enumerate() {
        let u = done.usage;
        // The replies of `forks` read one context, which the request counts once: the first reply fed it.
        if !split.forks || i == 0 {
            for (name, n) in [("prompt", u.prompt), ("kept", u.kept), ("fed", u.fed)] {
                add(&mut usage, name, n);
            }
        }
        add(&mut usage, "completion", u.completion);
        add(&mut usage, "reasoning", u.reasoning);
        let own = if split.forks { json!({"completion": u.completion, "reasoning": u.reasoning}) } else { serde_json::to_value(u).expect("usage is JSON") };
        let mut reply = match split.wanted {
            Wanted::Full => {
                let Value::Object(mut all) = serde_json::to_value(done).expect("done is JSON") else { unreachable!("done is an object") };
                all.remove("model");
                Value::Object(all)
            }
            Wanted::Ids => {
                let mut r = json!({"context": done.context, "finish": done.finish, "seed": done.seed});
                if let Some(phase) = done.phase {
                    r["phase"] = json!(phase);
                }
                r
            }
        };
        reply["index"] = json!(i);
        reply["usage"] = own;
        replies.push(reply);
    }
    json!({"model": dones.first().map(|d| d.model.clone()), "usage": usage, "replies": replies})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Usage;
    use crate::generation::{Finish, Phase};

    fn body(extra: Value) -> Value {
        let mut b = json!({"model": "m", "max_tokens": 8, "sample": {"temperature": 1.0, "seed": 7}, "context": {"thinking": false, "messages": [{"role": "user", "content": "u"}]}});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    }

    fn no_seed() -> u64 {
        panic!("the request has a seed")
    }

    #[test]
    fn a_plain_generation_is_not_split() {
        assert_eq!(split(&body(json!({})), 8, no_seed).unwrap(), None);
    }

    #[test]
    fn forks_give_one_plain_request_for_each_reply_with_the_seeds_in_order() {
        let s = split(&body(json!({"forks": 3, "replies": "ids"})), 8, no_seed).unwrap().unwrap();
        assert_eq!((s.requests.len(), s.wanted, s.forks), (3, Wanted::Ids, true));
        for (i, r) in s.requests.iter().enumerate() {
            let mut want = body(json!({}));
            want["sample"]["seed"] = json!(7 + i as u64);
            assert_eq!(*r, want);
            // Each one decodes as a plain generation.
            serde_json::from_value::<crate::generation::GenerationRequest>(r.clone()).unwrap();
        }
    }

    #[test]
    fn a_list_of_forks_gives_each_reply_its_own_sample_and_a_seed_of_its_own_stays() {
        let s = split(&body(json!({"forks": [{"temperature": 0.3}, {"temperature": 0.9, "seed": 100}, {}]})), 8, no_seed).unwrap().unwrap();
        let samples: Vec<&Value> = s.requests.iter().map(|r| &r["sample"]).collect();
        assert_eq!(samples, [&json!({"temperature": 0.3, "seed": 7}), &json!({"temperature": 0.9, "seed": 100}), &json!({"temperature": 1.0, "seed": 9})]);
        assert_eq!(s.wanted, Wanted::Full);
    }

    #[test]
    fn a_request_with_no_seed_takes_the_first_seed_of_the_server_and_seeds_wrap() {
        let mut b = body(json!({"forks": 2}));
        b["sample"] = json!({"temperature": 1.0});
        let s = split(&b, 8, || u64::MAX).unwrap().unwrap();
        assert_eq!((&s.requests[0]["sample"]["seed"], &s.requests[1]["sample"]["seed"]), (&json!(u64::MAX), &json!(0)));
        b.as_object_mut().unwrap().remove("sample");
        let s = split(&b, 8, || 5).unwrap().unwrap();
        assert_eq!(s.requests[1]["sample"], json!({"seed": 6}));
    }

    #[test]
    fn contexts_give_one_request_for_each_context() {
        let mut b = body(json!({"contexts": [{"parent": "p0"}, {"parent": "p1"}]}));
        b.as_object_mut().unwrap().remove("context");
        let s = split(&b, 8, no_seed).unwrap().unwrap();
        assert!(!s.forks);
        assert_eq!((&s.requests[0]["context"], &s.requests[1]["context"]), (&json!({"parent": "p0"}), &json!({"parent": "p1"})));
        assert_eq!((&s.requests[0]["sample"]["seed"], &s.requests[1]["sample"]["seed"]), (&json!(7), &json!(8)));
        assert!(s.requests[0].get("contexts").is_none());
    }

    #[test]
    fn each_refusal_names_its_member() {
        let without_context = |extra: Value| {
            let mut b = body(extra);
            b.as_object_mut().unwrap().remove("context");
            b
        };
        let cases = [
            (body(json!({"replies": "ids"})), "replies", "only beside"),
            (body(json!({"forks": 0})), "forks", "1 to 8"),
            (body(json!({"forks": 9})), "forks", "1 to 8"),
            (body(json!({"forks": []})), "forks", "1 to 8"),
            (body(json!({"forks": 1.5})), "forks", "a count or a list"),
            (body(json!({"forks": [3]})), "forks", "forks[0]"),
            (body(json!({"forks": 2, "contexts": []})), "forks", "only beside context"),
            (body(json!({"contexts": [{}]})), "contexts", "not both"),
            (without_context(json!({"forks": 2})), "context", "missing field `context`"),
            (without_context(json!({"contexts": []})), "contexts", "1 to 8"),
            (without_context(json!({"contexts": {}})), "contexts", "a list of contexts"),
            (body(json!({"forks": 2, "replies": "text"})), "replies", "\"full\" or \"ids\""),
            (body(json!({"forks": 2, "stream": true})), "stream", "takes no stream"),
            (body(json!({"forks": 2, "output": {"mime": "audio/wav"}})), "output", "takes no output"),
            (body(json!({"forks": 2, "decisions": [{"spec_id": "x", "when": "after"}]})), "decisions", "takes no decisions"),
        ];
        for (b, param, text) in cases {
            let e = split(&b, 8, no_seed).unwrap_err();
            assert_eq!(e.param.as_deref(), Some(param), "{b}");
            assert!(e.message.contains(text), "{}", e.message);
        }
        // A member that is off is allowed.
        split(&body(json!({"forks": 2, "stream": false, "decisions": []})), 8, no_seed).unwrap().unwrap();
    }

    #[test]
    fn an_error_of_a_context_goes_under_its_place_in_contexts() {
        for (param, want) in [("messages[2].content[1]", "contexts[1].messages[2].content[1]"), ("thinking", "contexts[1].thinking"), ("context.parent", "contexts[1].parent"), ("control[0].strength", "contexts[1].control[0].strength"), ("continue", "contexts[1].continue"), ("max_tokens", "max_tokens"), ("model", "model")] {
            assert_eq!(param_in(param, 1), want);
        }
    }

    fn done(completion: u64, fed: u64, phase: Option<Phase>) -> GenerationDone {
        GenerationDone {
            model: "m".into(),
            content: "text".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish: if phase.is_some() { Finish::MaxTokens } else { Finish::EndTurn },
            usage: Usage { prompt: 10, kept: 10 - fed, fed, completion, reasoning: 0, media: None },
            head: None,
            context: Some(crate::ids::ContextId::parse(format!("blake3:{}", "a".repeat(64))).unwrap()),
            snapshots: vec![],
            stats: None,
            decisions: vec![],
            seed: Some(7),
            phase,
            outputs: vec![],
        }
    }

    #[test]
    fn forks_count_their_context_once_and_contexts_count_each() {
        let dones = [done(3, 10, None), done(5, 0, Some(Phase::Content))];
        let forks = Split { requests: vec![], wanted: Wanted::Full, forks: true };
        let j = join(&forks, &dones);
        assert_eq!(j["usage"], json!({"prompt": 10, "kept": 0, "fed": 10, "completion": 8, "reasoning": 0}));
        assert_eq!((&j["replies"][1]["usage"], &j["replies"][1]["index"], &j["replies"][1]["content"]), (&json!({"completion": 5, "reasoning": 0}), &json!(1), &json!("text")));
        assert!(j["replies"][0].get("model").is_none());
        assert_eq!(j["model"], json!("m"));
        let contexts = Split { forks: false, ..forks };
        let j = join(&contexts, &dones);
        assert_eq!(j["usage"], json!({"prompt": 20, "kept": 10, "fed": 10, "completion": 8, "reasoning": 0}));
        assert_eq!(j["replies"][1]["usage"]["prompt"], json!(10));
    }

    #[test]
    fn ids_leave_the_text_out_and_keep_the_record_and_the_counts() {
        let s = Split { requests: vec![], wanted: Wanted::Ids, forks: true };
        let j = join(&s, &[done(3, 10, None), done(5, 0, Some(Phase::Content))]);
        let id = format!("blake3:{}", "a".repeat(64));
        assert_eq!(j["replies"][0], json!({"index": 0, "context": id, "finish": "end_turn", "seed": 7, "usage": {"completion": 3, "reasoning": 0}}));
        assert_eq!(j["replies"][1], json!({"index": 1, "context": id, "finish": "max_tokens", "phase": "content", "seed": 7, "usage": {"completion": 5, "reasoning": 0}}));
    }
}
