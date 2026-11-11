//! Context records: the context id, a context that names a `parent`, and the reply in a record.
//!
//! A server holds a record of each context it reads. A request names a record as the `parent`
//! of its context and carries only the messages after it. `resolve` turns that form into the
//! whole context before a request is decoded, so every later check reads a whole context, and
//! the two forms cannot differ.

use serde_json::{Map, Value};

use crate::context::Context;
use crate::error::Invalid;
use crate::generation::GenerationDone;
use crate::ids::ContextId;

/// The members that stand beside `parent`. A member of the first list replaces the parent's.
const REPLACES: [&str; 4] = ["thinking", "tools", "queue", "control"];

/// The text of a context's record: the context as compact JSON, with each member in the order
/// this crate writes it and a tool's `input_schema` and a call's `input` in the order they were
/// written. It is not RFC 8785: the template renders member order, so the order is content.
pub fn record_text(context: &Context) -> String {
    serde_json::to_string(context).expect("a context is JSON")
}

/// The id of a context: the content id of its record's text.
pub fn context_id(context: &Context) -> ContextId {
    ContextId::parse(crate::canon::content_id(record_text(context).as_bytes())).expect("a content id is a context id")
}

/// Why a context that names a `parent` did not become a whole context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unresolved {
    /// The request breaks the contract: a `400`.
    Invalid(Invalid),
    /// The server holds no record with this id: a `404` naming `param`. The client sends the
    /// context whole.
    NotHeld { param: String, id: ContextId },
}

impl Unresolved {
    /// The message of the `404` for a parent the server does not hold.
    pub fn not_held_message(id: &ContextId) -> String {
        format!("nothing was read: the server holds no context record {}; send the context whole, which holds its record again", id)
    }
}

/// Makes the context at `value` whole when it names a `parent`. `at` is the name of the context
/// in its request, such as `context` or `contexts[2]`. A context without `parent` stays as it
/// is. `close` says the request reads no open message: then a parent that ends with an open
/// message is read closed. Without `close`, it is closed only when messages follow it.
pub fn resolve(value: &mut Value, at: &str, close: bool, lookup: &dyn Fn(&ContextId) -> Option<Context>) -> Result<(), Unresolved> {
    let Some(sent) = value.as_object() else { return Ok(()) };
    let Some(parent) = sent.get("parent") else { return Ok(()) };
    let param = format!("{}.parent", at);
    let invalid = |param: String, message: String| Unresolved::Invalid(Invalid::param(param, message));
    let Some(text) = parent.as_str() else {
        return Err(invalid(param, format!("nothing was read: parent is a context id, not {}", parent)));
    };
    let id = ContextId::parse(text).map_err(|problem| invalid(param.clone(), format!("nothing was read: parent {:?} is {}", text, problem)))?;
    for name in sent.keys() {
        if name != "parent" && name != "messages" && !REPLACES.contains(&name.as_str()) {
            return Err(invalid(format!("{}.{}", at, name), format!("nothing was read: {} does not stand beside parent; the members are thinking, tools, queue, control, and messages", name)));
        }
    }
    let added = match sent.get("messages") {
        None => Vec::new(),
        Some(Value::Array(list)) => list.clone(),
        Some(other) => return Err(invalid(format!("{}.messages", at), format!("nothing was read: messages is a list of messages, not {}", other))),
    };
    let held = lookup(&id).ok_or(Unresolved::NotHeld { param, id })?;
    let Value::Object(mut whole) = serde_json::to_value(&held).expect("a context is JSON") else { unreachable!("a context is an object") };
    for name in REPLACES {
        match sent.get(name) {
            None => {}
            Some(Value::Null) if name == "queue" => {
                whole.remove(name);
            }
            Some(v) => {
                whole.insert(name.to_string(), v.clone());
            }
        }
    }
    let Some(Value::Array(messages)) = whole.get_mut("messages") else { unreachable!("a context has messages") };
    if (close || !added.is_empty())
        && let Some(Value::Object(last)) = messages.last_mut()
    {
        last.remove("continue");
    }
    messages.extend(added);
    *value = Value::Object(ordered(whole));
    Ok(())
}

/// The members of a context in the order this crate writes them, so the whole context reads the
/// same however it was sent.
fn ordered(mut whole: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for name in ["thinking", "tools", "queue", "control", "messages"] {
        if let Some(v) = whole.remove(name) {
            out.insert(name.to_string(), v);
        }
    }
    out
}

/// Makes each context of a request body whole: `context`, and each of `contexts`, `positive`,
/// and `negative`. `close` is as for [`resolve`]: `false` only for a generation.
pub fn resolve_request(body: &mut Value, close: bool, lookup: &dyn Fn(&ContextId) -> Option<Context>) -> Result<(), Unresolved> {
    let Some(members) = body.as_object_mut() else { return Ok(()) };
    if let Some(context) = members.get_mut("context") {
        resolve(context, "context", close, lookup)?;
    }
    for list in ["contexts", "positive", "negative"] {
        if let Some(Value::Array(contexts)) = members.get_mut(list) {
            for (i, context) in contexts.iter_mut().enumerate() {
                resolve(context, &format!("{}[{}]", list, i), close, lookup)?;
            }
        }
    }
    Ok(())
}

/// The context of a generation with its reply after it: what the record that `done.context`
/// names holds. The reply is an assistant message with `snap: true`. A reply cut by `max_tokens`
/// that can be continued is an open message. A reply to an open message takes its place.
pub fn with_reply(context: &Context, done: &GenerationDone) -> Context {
    let mut out = context.clone();
    if out.open().is_some() {
        out.messages.pop();
    }
    let reply = match done.as_open_message() {
        Some(open) => open,
        None => {
            // A call that did not parse goes back as text after the content, as the model wrote it.
            let mut parsed = done.clone();
            for call in &done.tool_calls {
                if let crate::generation::ReplyToolCall::Unparsed { raw, .. } = call {
                    parsed.content.push_str(raw);
                }
            }
            parsed.tool_calls.retain(|c| matches!(c, crate::generation::ReplyToolCall::Parsed { .. }));
            let mut closed = parsed.as_message().expect("each call that is left parsed");
            closed.snap = true;
            closed
        }
    };
    out.messages.push(reply);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Tool, Usage};
    use crate::generation::{Finish, Phase};
    use crate::message::Message;
    use serde_json::json;

    fn held() -> Context {
        Context { tools: vec![Tool::new("next", json!({"type": "object", "b": 1, "a": 2}))], queue: Some("next".into()), ..Context::new(true, vec![Message::system("s"), Message::user("u").snapped()]) }
    }

    fn lookup(context: &Context) -> impl Fn(&ContextId) -> Option<Context> + '_ {
        let id = context_id(context);
        move |asked| (*asked == id).then(|| context.clone())
    }

    fn done(content: &str, finish: Finish, phase: Option<Phase>) -> GenerationDone {
        GenerationDone {
            model: "m".into(),
            content: content.into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish,
            usage: Usage::default(),
            head: None,
            context: None,
            snapshots: vec![],
            stats: None,
            decisions: vec![],
            seed: None,
            phase,
            outputs: vec![],
        }
    }

    #[test]
    fn the_id_is_the_content_id_of_the_record_text_and_member_order_is_content() {
        let a = held();
        assert_eq!(context_id(&a).as_str(), crate::canon::content_id(record_text(&a).as_bytes()));
        assert_eq!(record_text(&a), r#"{"thinking":true,"tools":[{"name":"next","input_schema":{"type":"object","b":1,"a":2}}],"queue":"next","messages":[{"role":"system","content":"s"},{"role":"user","content":"u","snap":true}]}"#);
        // The same schema with its members in another order renders differently, so it is another context.
        let mut b = held();
        b.tools = vec![Tool::new("next", json!({"type": "object", "a": 2, "b": 1}))];
        assert_ne!(context_id(&a), context_id(&b));
        // A mark is content.
        let mut c = held();
        c.messages[1].snap = false;
        assert_ne!(context_id(&a), context_id(&c));
    }

    #[test]
    fn a_context_with_a_parent_is_the_parent_with_the_new_messages_after_it() {
        let parent = held();
        let mut sent = json!({"parent": context_id(&parent).as_str(), "messages": [{"role": "assistant", "content": "a"}, {"role": "user", "content": "more"}]});
        resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
        let mut whole = held();
        whole.messages.extend([Message::assistant("a"), Message::user("more")]);
        assert_eq!(serde_json::from_value::<Context>(sent.clone()).unwrap(), whole);
        // The two forms are one context: the same text, so the same id.
        assert_eq!(serde_json::to_string(&sent).unwrap(), record_text(&whole));
    }

    #[test]
    fn a_parent_alone_is_the_parent_and_a_whole_context_stays_as_it_is() {
        let parent = held();
        for mut sent in [json!({"parent": context_id(&parent).as_str()}), json!({"parent": context_id(&parent).as_str(), "messages": []})] {
            resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
            assert_eq!(serde_json::from_value::<Context>(sent).unwrap(), parent);
        }
        let whole = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
        let mut sent = whole.clone();
        resolve(&mut sent, "context", false, &|_| panic!("a whole context asks for no record")).unwrap();
        assert_eq!(sent, whole);
    }

    #[test]
    fn a_member_beside_parent_replaces_the_parents() {
        let parent = held();
        let id = context_id(&parent);
        let mut sent = json!({"parent": id.as_str(), "thinking": false, "tools": [], "queue": null, "control": [{"vector": format!("blake3:{}", "3".repeat(64)), "op": "add", "strength": 0.5}]});
        resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
        let got: Context = serde_json::from_value(sent).unwrap();
        assert!(!got.thinking);
        assert!(got.tools.is_empty());
        assert_eq!(got.queue, None);
        assert_eq!(got.control.len(), 1);
        assert_eq!(got.messages, parent.messages);
        // A member that the request does not have is the parent's.
        let mut sent = json!({"parent": id.as_str(), "thinking": false});
        resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
        let got: Context = serde_json::from_value(sent).unwrap();
        assert_eq!((got.tools, got.queue), (parent.tools.clone(), parent.queue.clone()));
    }

    #[test]
    fn a_parent_that_is_not_held_names_its_member_and_its_id() {
        let parent = held();
        let other = format!("blake3:{}", "e".repeat(64));
        let mut sent = json!({"parent": other, "messages": []});
        let e = resolve(&mut sent, "contexts[2]", false, &lookup(&parent)).unwrap_err();
        assert_eq!(e, Unresolved::NotHeld { param: "contexts[2].parent".into(), id: ContextId::parse(other.clone()).unwrap() });
        assert!(Unresolved::not_held_message(&ContextId::parse(other.clone()).unwrap()).contains(&other));
    }

    #[test]
    fn a_bad_parent_or_a_member_that_does_not_stand_beside_it_is_invalid() {
        let parent = held();
        let id = context_id(&parent);
        let cases = [
            (json!({"parent": "blake3:ab"}), "context.parent", "64"),
            (json!({"parent": 7}), "context.parent", "not 7"),
            (json!({"parent": id.as_str(), "model": "m"}), "context.model", "does not stand beside parent"),
            (json!({"parent": id.as_str(), "messages": {"role": "user"}}), "context.messages", "a list of messages"),
        ];
        for (mut sent, param, text) in cases {
            match resolve(&mut sent, "context", false, &lookup(&parent)).unwrap_err() {
                Unresolved::Invalid(e) => {
                    assert_eq!(e.param.as_deref(), Some(param));
                    assert!(e.message.contains(text), "{}", e.message);
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn an_open_parent_continues_alone_in_a_generation_and_closes_otherwise() {
        let mut parent = Context::new(false, vec![Message::user("u"), Message::assistant("so far").opened()]);
        let id = context_id(&parent);
        let alone = json!({"parent": id.as_str()});
        // A generation with no new messages continues the open message.
        let mut sent = alone.clone();
        resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
        assert!(serde_json::from_value::<Context>(sent).unwrap().open().is_some());
        // Another request reads it closed.
        let mut sent = alone.clone();
        resolve(&mut sent, "context", true, &lookup(&parent)).unwrap();
        let closed: Context = serde_json::from_value(sent).unwrap();
        parent.messages[1].open = false;
        assert_eq!(closed, parent);
        // A message after it closes it too.
        parent.messages[1].open = true;
        let mut sent = json!({"parent": id.as_str(), "messages": [{"role": "user", "content": "next"}]});
        resolve(&mut sent, "context", false, &lookup(&parent)).unwrap();
        let got: Context = serde_json::from_value(sent).unwrap();
        assert!(!got.messages[1].open);
        assert_eq!(got.messages.len(), 3);
        got.check().unwrap();
    }

    #[test]
    fn a_request_resolves_each_of_its_contexts_and_names_the_one_that_fails() {
        let parent = held();
        let id = context_id(&parent);
        let mut body = json!({"model": "m", "context": {"parent": id.as_str()}, "contexts": [{"parent": id.as_str()}, {"thinking": false, "messages": []}],
                              "positive": [{"parent": id.as_str()}], "negative": [{"parent": id.as_str()}]});
        resolve_request(&mut body, true, &lookup(&parent)).unwrap();
        let whole = serde_json::to_value(&parent).unwrap();
        assert_eq!(body["context"], whole);
        assert_eq!(body["contexts"][0], whole);
        assert_eq!(body["contexts"][1], json!({"thinking": false, "messages": []}));
        assert_eq!((&body["positive"][0], &body["negative"][0]), (&whole, &whole));
        let gone = format!("blake3:{}", "0".repeat(64));
        for (list, param) in [("contexts", "contexts[1].parent"), ("positive", "positive[1].parent"), ("negative", "negative[1].parent")] {
            let mut body = json!({list: [{"parent": id.as_str()}, {"parent": gone}]});
            match resolve_request(&mut body, true, &lookup(&parent)).unwrap_err() {
                Unresolved::NotHeld { param: got, .. } => assert_eq!(got, param),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_record_ends_with_the_reply_as_a_marked_assistant_message() {
        let context = Context::new(false, vec![Message::user("u")]);
        let got = with_reply(&context, &done("hello", Finish::EndTurn, None));
        assert_eq!(got.messages, vec![Message::user("u"), Message::assistant("hello").snapped()]);
        assert_eq!(got.thinking, context.thinking);
    }

    #[test]
    fn a_call_that_did_not_parse_is_text_after_the_content() {
        use crate::generation::ReplyToolCall;
        let context = Context::new(false, vec![Message::user("u")]);
        let mut d = done("Listing.", Finish::ToolUse, None);
        d.tool_calls = vec![
            ReplyToolCall::Parsed { id: "c0".into(), name: "shell".into(), input: Map::new(), stats: None },
            ReplyToolCall::Unparsed { id: "c1".into(), name: None, raw: "sh(".into(), error: "e".into(), stats: None },
        ];
        let reply = with_reply(&context, &d).messages.pop().unwrap();
        assert_eq!(reply.text(), Some("Listing.sh("));
        assert_eq!(reply.tool_calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c0"]);
        assert!(reply.snap);
    }

    #[test]
    fn a_cut_reply_is_an_open_message_and_a_reply_to_an_open_message_takes_its_place() {
        let context = Context::new(false, vec![Message::user("u")]);
        let cut = with_reply(&context, &done("hel", Finish::MaxTokens, Some(Phase::Content)));
        assert_eq!(cut.messages[1], Message::assistant("hel").opened());
        // The continuation's `done` is the whole message, so the record holds one reply, not two.
        let whole = with_reply(&cut, &done("hello", Finish::EndTurn, None));
        assert_eq!(whole.messages, vec![Message::user("u"), Message::assistant("hello").snapped()]);
        // A reply cut inside a call cannot be continued: it is a closed message.
        let call = with_reply(&context, &done("x", Finish::MaxTokens, Some(Phase::Call)));
        assert_eq!(call.messages[1], Message::assistant("x").snapped());
    }
}
