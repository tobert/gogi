use std::io::{Read, Write};
use std::net::TcpStream;

use gogi_fake::{Config, FakeModel, ModelStatus, Server};
use gogi_proto::decision::DecisionResponse;
use gogi_proto::generation::{GenerationDone, Phase, StreamEvent};
use gogi_proto::ids::{REQUEST_ID_HEADER, is_request_id};
use serde_json::{Value, json};

/// The status, the `request-id` header, and the raw body.
fn exchange(s: &Server, method: &str, path: &str, body: Option<Value>) -> (u16, String, String) {
    let addr = s.url().trim_start_matches("http://").to_string();
    let mut c = TcpStream::connect(&addr).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    write!(c, "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).unwrap();
    let mut raw = String::new();
    c.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let id = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(REQUEST_ID_HEADER)).map(|(_, v)| v.trim().to_string())).unwrap_or_default();
    (code, id, body.to_string())
}

fn call(s: &Server, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let (code, _, body) = exchange(s, method, path, body);
    (code, serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// The events of a streamed generation.
fn stream(s: &Server, mut body: Value) -> Vec<StreamEvent> {
    body["stream"] = json!(true);
    let (code, _, raw) = exchange(s, "POST", "/council/v2/generations", Some(body));
    assert_eq!(code, 200, "{raw}");
    raw.split("\n\n")
        .filter_map(|e| {
            let name = e.lines().find_map(|l| l.strip_prefix("event: "))?;
            let data = e.lines().find_map(|l| l.strip_prefix("data: "))?;
            StreamEvent::parse(name, data).unwrap()
        })
        .collect()
}

fn post(s: &Server, path: &str, body: Value) -> (u16, Value) {
    call(s, "POST", path, Some(body))
}

fn ctx(messages: Value) -> Value {
    json!({"thinking": true, "messages": messages})
}

fn answer_with_reasoning() -> Vec<Value> {
    vec![
        json!({"role": "system", "content": "s"}),
        json!({"role": "user", "content": "u", "snap": true}),
        json!({"role": "assistant", "content": "a", "reasoning": "r", "snap": true}),
    ]
}

#[test]
fn a_second_build_feeds_nothing_and_an_extension_feeds_only_the_new_tokens() {
    let s = Server::start(Config::default()).unwrap();
    let mut m = answer_with_reasoning();
    let (_, a) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m))}));
    assert_eq!((a["usage"]["kept"].as_u64(), a["from"].clone()), (Some(0), Value::Null));
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m))}));
    assert_eq!((b["usage"]["fed"].as_u64(), &b["head"]), (Some(0), &a["head"]));
    m[2] = json!({"role": "assistant", "content": "a", "reasoning": "r", "snap": true, "tool_calls": [{"id": "c", "name": "sh", "input": {}}]});
    m.push(json!({"role": "tool", "tool_call_id": "c", "content": "out"}));
    let (st, c) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m))}));
    assert_eq!(st, 200, "{c}");
    let u: gogi_proto::Usage = serde_json::from_value(c["usage"].clone()).unwrap();
    u.check().unwrap();
    assert!(u.kept > 0 && u.fed < u.prompt && u.completion == 0, "{c}");
}

#[test]
fn a_text_answers_boundary_goes_when_a_user_message_follows_and_a_call_keeps_its_own() {
    let s = Server::start(Config::default()).unwrap();
    let mut m = answer_with_reasoning();
    let (_, a) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m))}));
    assert_eq!(a["snapshots"].as_array().unwrap().iter().filter(|x| x["message"] == 2).count(), 1);
    m.push(json!({"role": "user", "content": "next", "snap": true}));
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m))}));
    let msgs: Vec<u64> = b["snapshots"].as_array().unwrap().iter().map(|x| x["message"].as_u64().unwrap()).collect();
    assert_eq!(msgs, [1, 3], "message 2 lost its boundary: {b}");
    // The user boundary is still reused.
    assert_eq!(b["from"], a["snapshots"][0]["id"]);

    let mut calls = answer_with_reasoning();
    calls[2] = json!({"role": "assistant", "reasoning": "r", "snap": true, "tool_calls": [{"id": "c", "name": "sh", "input": {}}]});
    calls.push(json!({"role": "tool", "tool_call_id": "c", "content": "out"}));
    calls.push(json!({"role": "user", "content": "next", "snap": true}));
    let (_, c) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(calls))}));
    assert!(c["snapshots"].as_array().unwrap().iter().any(|x| x["message"] == 2), "{c}");
}

#[test]
fn a_dry_run_builds_nothing() {
    let s = Server::start(Config::default()).unwrap();
    let m = answer_with_reasoning();
    let (_, d) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(m)), "dry_run": true}));
    assert_eq!((d["dry_run"].clone(), d["snapshots"].clone(), d["usage"]["kept"].as_u64()), (json!(true), json!([]), Some(0)));
    let (code, _) = call(&s, "GET", &format!("/council/v2/snapshots/{}", d["head"].as_str().unwrap()), None);
    assert_eq!(code, 404);
}

#[test]
fn a_generation_replays_bit_for_bit_and_its_reply_extends_into_the_held_head() {
    let s = Server::start(Config::default()).unwrap();
    let body = json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "hi", "snap": true}])), "max_tokens": 40, "sample": {"temperature": 0}});
    let (_, a) = post(&s, "/council/v2/generations", body.clone());
    let (_, b) = post(&s, "/council/v2/generations", body);
    assert_eq!((a["content"].clone(), a["reasoning"].clone(), a["head"].clone()), (b["content"].clone(), b["reasoning"].clone(), b["head"].clone()));
    assert_ne!(a["head"], Value::Null);
    // Sending the reply back lands on the head the generation reported.
    let mut reply = json!({"role": "assistant", "content": a["content"]});
    if a["reasoning"] != json!("") {
        reply["reasoning"] = a["reasoning"].clone();
    }
    let (_, built) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "hi", "snap": true}, reply]))}));
    assert_eq!(built["head"], a["head"]);
    assert_eq!(built["usage"]["fed"], 0);
}

#[test]
fn a_stream_sends_token_events_then_done_with_the_same_reply() {
    let s = Server::start(Config::default()).unwrap();
    let body = json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "hi"}])), "max_tokens": 12, "sample": {"temperature": 0}});
    let (_, unstreamed) = post(&s, "/council/v2/generations", body.clone());
    let unstreamed: GenerationDone = serde_json::from_value(unstreamed).unwrap();
    let events = stream(&s, body);
    assert_eq!(events[0], StreamEvent::Ping, "the fake sends one ping first");
    let Some(StreamEvent::Done(done)) = events.last() else { panic!("{events:?}") };
    let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t) } else { None }).collect();
    assert_eq!(tokens.len() as u64, unstreamed.usage.completion);
    assert_eq!(tokens.last().unwrap().n, done.usage.completion);
    assert_eq!(tokens.iter().filter(|t| t.phase == Phase::Reasoning).count() as u64, done.usage.reasoning);
    assert_eq!((&done.content, done.usage.completion), (&unstreamed.content, unstreamed.usage.completion));
    done.usage.check().unwrap();
}

#[test]
fn finish_says_why_the_reply_ended() {
    let s = Server::start(Config::default()).unwrap();
    let tool = json!({"name": "next", "input_schema": {"type": "object"}});
    let generate = |content: &str, max_tokens: u64| {
        let (code, d) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": {"thinking": false, "tools": [tool], "messages": [{"role": "user", "content": content}]}, "max_tokens": max_tokens, "sample": {"temperature": 0}}));
        assert_eq!(code, 200, "{d}");
        serde_json::from_value::<GenerationDone>(d).unwrap()
    };
    let called = generate("CALL it", 64);
    assert_eq!((called.finish, called.tool_calls.len()), (gogi_proto::generation::Finish::ToolUse, 1));
    assert_eq!(serde_json::to_value(&called.tool_calls[0]).unwrap()["input"], json!({}));
    assert_eq!(generate("hi", 1).finish, gogi_proto::generation::Finish::MaxTokens);
    let small = Server::start(Config { context_tokens: 12, ..Config::default() }).unwrap();
    let (_, d) = post(&small, "/council/v2/generations", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "hi"}])), "max_tokens": 12, "sample": {"temperature": 0.0, "seed": 3}}));
    assert_eq!(d["finish"], "context_full", "{d}");
    assert_eq!(d["seed"], 3);
}

#[test]
fn a_reply_that_fails_midway_ends_its_stream_with_an_error_event() {
    let s = Server::start(Config::default()).unwrap();
    let body = json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "FAIL"}])), "max_tokens": 12});
    let events = stream(&s, body.clone());
    let Some(StreamEvent::Error(e)) = events.last() else { panic!("{events:?}") };
    assert!(is_request_id(e.request_id.as_deref().unwrap()));
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Done(_))));
    let (code, e) = post(&s, "/council/v2/generations", body);
    assert_eq!((code, &e["error"]["type"]), (500, &json!("internal")));
}

#[test]
fn every_response_carries_a_request_id_and_an_error_repeats_it() {
    let s = Server::start(Config::default()).unwrap();
    let (code, a, _) = exchange(&s, "GET", "/council/v2/identity", None);
    let (_, b, _) = exchange(&s, "GET", "/council/v2/identity", None);
    assert_eq!(code, 200);
    assert!(is_request_id(&a) && is_request_id(&b) && a != b, "{a} {b}");
    let (code, id, body) = exchange(&s, "POST", "/council/v2/decisions", Some(json!({"model": "toy", "spec_id": format!("blake3:{}", "a".repeat(64)), "case": "x", "verdict": "A"})));
    let e: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((code, &e["error"]["param"], &e["request_id"]), (400, &json!("verdict"), &json!(id)), "{e}");
    let (code, id, _) = exchange(&s, "DELETE", "/council/v2/pins/never", None);
    assert_eq!((code, is_request_id(&id)), (404, true));
}

#[test]
fn a_failed_tool_result_changes_the_rendering() {
    let s = Server::start(Config::default()).unwrap();
    let m = |is_error: bool| json!([{"role": "user", "content": "u"}, {"role": "assistant", "tool_calls": [{"id": "c", "name": "sh", "input": {}}]},
        {"role": "tool", "tool_call_id": "c", "content": "exit 1", "is_error": is_error}]);
    let (_, ok) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(m(false))}));
    let (_, failed) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(m(true))}));
    assert_ne!(ok["head"], failed["head"]);
    assert_eq!(ok["usage"]["prompt"].as_u64().unwrap() + 1, failed["usage"]["prompt"].as_u64().unwrap());
}

#[test]
fn a_prompt_with_no_room_is_a_413_context_full_and_a_bad_context_a_400() {
    let s = Server::start(Config { context_tokens: 64, ..Config::default() }).unwrap();
    let long = json!([{"role": "user", "content": "x".repeat(200)}]);
    let (code, e) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": ctx(long.clone()), "max_tokens": 8}));
    assert_eq!((code, e["error"]["type"].clone()), (413, json!("context_full")));
    assert!(e["error"]["prompt_tokens"].as_u64().unwrap() > e["error"]["context_tokens"].as_u64().unwrap());
    let (code, e) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(long)}));
    assert_eq!((code, &e["error"]["type"], &e["error"]["param"]), (413, &json!("too_large"), &json!("context_tokens")), "a context past the limit is too large, not a prompt with no room: {e}");
    let (code, _) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": ctx(json!([{"role": "tool", "tool_call_id": "nope", "content": "x"}])), "max_tokens": 8}));
    assert_eq!(code, 400);
    let (code, e) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "x"}]))}));
    assert_eq!((code, &e["error"]["param"]), (400, &json!("max_tokens")), "a generation sends max_tokens: {e}");
    let (code, _) = call(&s, "PUT", "/council/v2/contexts/old", Some(json!({})));
    assert_eq!(code, 404);
}

#[test]
fn pins_name_their_snapshot_not_its_chain_expire_and_report_unheld_ids() {
    let s = Server::start(Config::default()).unwrap();
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(answer_with_reasoning()))}));
    let (first, head) = (b["snapshots"][0]["id"].as_str().unwrap(), b["head"].as_str().unwrap());
    let unheld = format!("blake3:{}", "0".repeat(64));
    let (code, p) = call(&s, "PUT", "/council/v2/pins/o", Some(json!({"snapshots": [head, unheld], "ttl_s": 60})));
    assert_eq!(code, 200);
    assert_eq!((p["snapshots"][0]["held"].clone(), p["snapshots"][1]["held"].clone()), (json!(true), json!(false)));
    let (_, named) = call(&s, "GET", &format!("/council/v2/snapshots/{head}"), None);
    let (_, chain) = call(&s, "GET", &format!("/council/v2/snapshots/{first}"), None);
    assert_eq!((named["pins"][0]["owner"].clone(), chain["pins"].clone()), (json!("o"), json!([])));
    for ttl in [0, 86401] {
        assert_eq!(call(&s, "PUT", "/council/v2/pins/o", Some(json!({"snapshots": [], "ttl_s": ttl}))).0, 400);
    }
    s.advance_clock(61);
    assert_eq!(call(&s, "GET", "/council/v2/pins/o", None).0, 404, "the pin ran out");
    assert_eq!(call(&s, "GET", "/council/v2/pins/never", None).0, 404);
    assert_eq!(call(&s, "DELETE", "/council/v2/pins/never", None).0, 404);
}

#[test]
fn pins_past_pin_bytes_are_a_413_and_pin_nothing() {
    let s = Server::start(Config { pin_bytes: 64, ..Config::default() }).unwrap();
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!(answer_with_reasoning()))}));
    let (code, e) = call(&s, "PUT", "/council/v2/pins/o", Some(json!({"snapshots": [b["head"]], "ttl_s": 60})));
    assert_eq!((code, e["error"]["param"].clone()), (413, json!("pin_bytes")));
    assert_eq!(call(&s, "GET", "/council/v2/pins/o", None).0, 404);
}

#[test]
fn specs_and_pins_survive_a_restart_and_unpinned_snapshots_do_not() {
    let dir = std::env::temp_dir().join(format!("gogi-fake-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Server::start(Config { state_dir: Some(dir.clone()), ..Config::default() }).unwrap();
    let spec = json!({"name": "g", "instructions": "i", "input_label": "x", "questions": [{"id": "v", "type": "noul", "instructions": "fine?"}]});
    let (_, held) = post(&s, "/council/v2/specs", spec.clone());
    let (_, pinned) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "pin me"}]))}));
    let (_, loose) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "forget me"}]))}));
    call(&s, "PUT", "/council/v2/pins/o", Some(json!({"snapshots": [pinned["head"]], "ttl_s": 600})));
    s.restart().unwrap();
    assert_eq!(call(&s, "GET", &format!("/council/v2/specs/{}", held["spec_id"].as_str().unwrap()), None).0, 200);
    assert_eq!(call(&s, "GET", "/council/v2/pins/o", None).1["snapshots"], json!([pinned["head"]]));
    assert_eq!(call(&s, "GET", &format!("/council/v2/snapshots/{}", pinned["head"].as_str().unwrap()), None).0, 200);
    assert_eq!(call(&s, "GET", &format!("/council/v2/snapshots/{}", loose["head"].as_str().unwrap()), None).0, 404);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_spec_is_held_by_content_and_a_decision_decodes_and_verifies() {
    let s = Server::start(Config::default()).unwrap();
    let spec = json!({"name": "g", "instructions": "i", "input_label": "x", "questions": [
        {"id": "verdict", "type": "choice", "instructions": "what", "criteria": [{"option": "allow", "means": "a"}, {"option": "ask", "means": "b"}, {"option": "no", "means": "c"}]},
        {"id": "undo", "type": "score", "instructions": "how", "criteria": ["easy", "hard"]},
        {"id": "risky", "type": "noul", "instructions": "risky?"}]});
    let (_, a) = post(&s, "/council/v2/specs", spec.clone());
    let reordered: Value = serde_json::from_str(&serde_json::to_string(&spec).unwrap().replace(r#""name":"g","instructions":"i""#, r#""instructions":"i","name":"g""#)).unwrap();
    let (_, b) = post(&s, "/council/v2/specs", reordered);
    assert_eq!(a["spec_id"], b["spec_id"]);
    assert_eq!(a["spec_id"], json!(gogi_proto::spec_id_of(&spec).unwrap()));
    let req = json!({"model": "toy", "spec_id": a["spec_id"], "case": "rm -rf build", "pool": {"method": "loglinear", "weights": "mass"},
        "contexts": [ctx(json!([{"role": "system", "content": "s"}, {"role": "user", "content": "u", "snap": true}])), ctx(json!([{"role": "user", "content": "other"}]))]});
    let (code, d) = post(&s, "/council/v2/decisions", req.clone());
    assert_eq!(code, 200, "{d}");
    let resp: DecisionResponse = serde_json::from_value(d.clone()).unwrap();
    let parsed: gogi_proto::decision::DecisionRequest = serde_json::from_value(req.clone()).unwrap();
    gogi_proto::math::verify(&resp, &parsed.expect()).unwrap();
    let (_, again) = post(&s, "/council/v2/decisions", req);
    for (x, y) in d["reads"].as_array().unwrap().iter().zip(again["reads"].as_array().unwrap()) {
        assert_eq!(x["answers"], y["answers"], "replay is bit for bit");
    }
    assert_eq!(d["reads"][1]["context"], json!(1));
    assert!(resp.usage.kept + resp.usage.fed == resp.usage.prompt && resp.usage.fed > 0, "{:?}", resp.usage);
    assert_eq!(post(&s, "/council/v2/decisions", json!({"model": "toy", "spec_id": format!("blake3:{}", "a".repeat(64)), "case": "x"})).0, 404);
    assert_eq!(post(&s, "/council/v2/decisions", json!({"model": "toy", "spec_id": a["spec_id"], "case": "x", "contexts": [{"id": "old"}]})).0, 400);
    assert_eq!(call(&s, "DELETE", &format!("/council/v2/specs/{}", a["spec_id"].as_str().unwrap()), None).0, 204);
}

#[test]
fn a_queue_context_gets_the_case_as_the_calls_result() {
    let s = Server::start(Config::default()).unwrap();
    let spec = json!({"name": "g", "instructions": "i", "input_label": "x", "questions": [
        {"id": "verdict", "type": "choice", "instructions": "what", "criteria": [{"option": "allow", "means": "a"}, {"option": "ask", "means": "b"}]},
        {"id": "undo", "type": "score", "instructions": "how", "criteria": ["easy", "hard", "never"]}]});
    let (_, held) = post(&s, "/council/v2/specs", spec);
    let tool = |undo: &[&str]| json!({"name": "next", "input_schema": {"type": "object", "properties": {"answers": {"type": "object", "properties": {
        "verdict": {"type": "string", "enum": ["A", "B"]}, "undo": {"type": "string", "enum": undo}}}}}});
    let queue = |undo: &[&str]| json!({"thinking": true, "tools": [tool(undo)], "queue": "next", "messages": [
        {"role": "system", "content": "s"}, {"role": "user", "content": "go", "snap": true},
        {"role": "assistant", "tool_calls": [{"id": "c0", "name": "next", "input": {}}]}]});
    let req = |c: Value| json!({"model": "toy", "spec_id": held["spec_id"], "case": "git push", "contexts": [c]});
    let (code, d) = post(&s, "/council/v2/decisions", req(queue(&["A", "B", "C"])));
    assert_eq!(code, 200, "{d}");
    let resp: DecisionResponse = serde_json::from_value(d).unwrap();
    assert_eq!(resp.reads.len(), 1);
    let (code, e) = post(&s, "/council/v2/decisions", req(queue(&["A", "B"])));
    assert_eq!((code, e["error"]["param"].clone()), (400, json!("queue")));
}

#[test]
fn reads_are_per_letter_bit_for_bit_and_report_control_text_in_suffixes() {
    let s = Server::start(Config::default()).unwrap();
    let body = json!({"model": "toy", "contexts": [ctx(json!([{"role": "user", "content": "u <end>", "snap": true}]))], "suffixes": ["<asst>A", "plain"], "letters": ["A", "B", "C"], "top": 3});
    let (code, a) = post(&s, "/council/v2/reads", body.clone());
    assert_eq!(code, 200, "{a}");
    let (_, b) = post(&s, "/council/v2/reads", body);
    assert_eq!(a["reads"], b["reads"]);
    assert_eq!(a["reads"].as_array().unwrap().len(), 2);
    assert_eq!(a["reads"][0]["top"].as_array().unwrap().len(), 3);
    let u: gogi_proto::Usage = serde_json::from_value(a["usage"].clone()).unwrap();
    assert!(u.check().is_ok() && u.completion == 0 && u.kept > 0, "{u:?}");
    assert_eq!(a["signals"]["control_text"], json!([{"where": "suffix:0", "token": "<asst>"}]));
    let lp: Vec<f64> = ["A", "B", "C"].iter().map(|l| a["reads"][1]["logprobs"][*l].as_f64().unwrap()).collect();
    assert!((a["reads"][1]["mass"].as_f64().unwrap() - gogi_proto::math::log_sum_exp(&lp)).abs() < 1e-12);
    let (code, _) = post(&s, "/council/v2/reads", json!({"model": "toy", "contexts": [ctx(json!([{"role": "user", "content": "u"}]))], "suffixes": ["x"], "letters": ["AB", "C"]}));
    assert_eq!(code, 400);
}

fn day_and_night() -> Server {
    let night = FakeModel { aliases: vec!["night".into()], status: ModelStatus::Unloaded, ..FakeModel::ready("gogi-toy-night", 7) };
    Server::start(Config { models: vec![FakeModel::ready("gogi-toy-day", 0), night], ..Config::default() }).unwrap()
}

#[test]
fn with_two_served_models_a_request_names_one() {
    let s = day_and_night();
    let c = ctx(json!([{"role": "user", "content": "u", "snap": true}]));
    let (code, e) = post(&s, "/council/v2/contexts", json!({"context": c}));
    assert_eq!((code, &e["error"]["param"]), (400, &json!("model")), "a request names its model: {e}");
    let (code, e) = post(&s, "/council/v2/contexts", json!({"model": "gogi-toy-1", "context": c}));
    assert_eq!((code, &e["error"]["param"]), (400, &json!("model")), "{e}");
    let (code, e) = post(&s, "/council/v2/reads", json!({"model": "night", "contexts": [c], "suffixes": ["x"], "letters": ["A", "B"]}));
    assert_eq!((code, &e["error"]["type"], &e["error"]["model"], &e["error"]["status"]), (503, &json!("model_unavailable"), &json!("gogi-toy-night"), &json!("unloaded")), "{e}");
    let (code, day) = post(&s, "/council/v2/contexts", json!({"model": "gogi-toy-day", "context": c}));
    assert_eq!((code, &day["model"]), (200, &json!("gogi-toy-day")), "{day}");
    let (_, snap) = call(&s, "GET", &format!("/council/v2/snapshots/{}", day["head"].as_str().unwrap()), None);
    assert_eq!(snap["model"], "gogi-toy-day");
}

#[test]
fn each_served_model_has_its_own_snapshots_and_numbers_and_keeps_them_across_an_unload() {
    let s = day_and_night();
    s.set_status("gogi-toy-night", ModelStatus::Ready).unwrap();
    let c = ctx(json!([{"role": "user", "content": "u", "snap": true}]));
    let read = |model: &str| post(&s, "/council/v2/reads", json!({"model": model, "contexts": [c], "suffixes": ["x"], "letters": ["A", "B"]}));
    let ((_, day), (_, night)) = (read("gogi-toy-day"), read("night"));
    assert_eq!(night["model"], "gogi-toy-night", "an alias is answered with the name");
    assert_ne!(day["reads"][0]["snapshot"], night["reads"][0]["snapshot"]);
    assert_ne!(day["reads"][0]["logprobs"], night["reads"][0]["logprobs"]);
    s.set_status("gogi-toy-night", ModelStatus::Unloaded).unwrap();
    assert_eq!(read("night").0, 503);
    s.set_status("gogi-toy-night", ModelStatus::Ready).unwrap();
    let (_, again) = read("night");
    assert_eq!(again["reads"], night["reads"]);
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "night", "context": c}));
    assert_eq!(b["usage"]["fed"], 0, "the snapshot outlived the unload: {b}");
}

#[test]
fn a_streamed_reply_labels_each_token_by_its_part_and_the_parts_join_in_done() {
    let s = Server::start(Config::default()).unwrap();
    let tool = json!({"name": "next", "input_schema": {"type": "object"}});
    let body = json!({"model": "toy", "context": {"thinking": true, "tools": [tool], "messages": [{"role": "user", "content": "CALL it"}]}, "max_tokens": 400, "sample": {"temperature": 1, "seed": 5}, "stream": true});
    let events = stream(&s, body);
    let Some(StreamEvent::Done(done)) = events.last() else { panic!("{events:?}") };
    let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t) } else { None }).collect();
    let part = |p: Phase| tokens.iter().filter(|t| t.phase == p).map(|t| t.piece.as_str()).collect::<String>();
    assert!(!done.reasoning.is_empty(), "the thought wrote text: {done:?}");
    assert_eq!(part(Phase::Reasoning), done.reasoning);
    assert_eq!(part(Phase::Content), done.content);
    assert_eq!(part(Phase::Call), "<call>next({})<endcall>");
    let order: Vec<Phase> = tokens.iter().map(|t| t.phase).collect();
    let first_call = order.iter().position(|p| *p == Phase::Call).unwrap();
    assert!(order[..first_call].iter().filter(|p| **p == Phase::Reasoning).count() as u64 == done.usage.reasoning, "reasoning comes before the call");
}

#[test]
fn riding_decisions_are_a_400_naming_decisions_because_the_fake_does_not_declare_ride() {
    let s = Server::start(Config::default()).unwrap();
    let (_, id) = call(&s, "GET", "/council/v2/identity", None);
    assert!(!id["models"][0]["capabilities"].as_array().unwrap().contains(&json!("ride")));
    let body = json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": "hi"}])), "max_tokens": 4,
        "decisions": [{"spec_id": format!("blake3:{}", "a".repeat(64)), "when": "after"}]});
    let (code, e) = post(&s, "/council/v2/generations", body);
    assert_eq!((code, &e["error"]["param"]), (400, &json!("decisions")), "{e}");
}

// ---- continuations

fn generate(s: &Server, context: Value, max_tokens: u64) -> GenerationDone {
    let (code, d) = post(s, "/council/v2/generations", json!({"model": "toy", "context": context, "max_tokens": max_tokens, "sample": {"temperature": 0}}));
    assert_eq!(code, 200, "{d}");
    serde_json::from_value(d).unwrap()
}

fn thinking(messages: Vec<Value>) -> Value {
    json!({"thinking": true, "messages": messages})
}

fn opening() -> Vec<Value> {
    vec![json!({"role": "system", "content": "s"}), json!({"role": "user", "content": "go on", "snap": true})]
}

fn refused_param(s: &Server, path: &str, body: Value) -> Value {
    let (code, e) = post(s, path, body);
    assert_eq!(code, 400, "{e}");
    e["error"]["param"].clone()
}

#[test]
fn the_fake_declares_continuation() {
    let s = Server::start(Config::default()).unwrap();
    let (_, id) = call(&s, "GET", "/council/v2/identity", None);
    assert!(id["models"][0]["capabilities"].as_array().unwrap().contains(&json!("continuation")), "{id}");
}

#[test]
fn a_cut_reply_sent_back_open_resumes_from_its_head_and_writes_what_the_whole_reply_would() {
    let s = Server::start(Config::default()).unwrap();
    let whole = generate(&s, thinking(opening()), 300);
    assert!(whole.usage.completion > 4 && whole.phase.is_none(), "{whole:?}");
    let mut phases = Vec::new();
    for k in 1..whole.usage.completion.min(30) {
        let cut = generate(&s, thinking(opening()), k);
        assert_eq!(cut.finish, gogi_proto::generation::Finish::MaxTokens);
        let head = cut.head.clone().expect("a cut reply holds the head of its open rendering");
        let last = cut.snapshots.last().unwrap();
        assert_eq!((&last.id, last.tokens), (&head, cut.usage.prompt + k), "the head is listed last and holds the prompt and the sampled tokens");
        phases.push(cut.phase.unwrap());
        let open = cut.as_open_message().unwrap();
        let mut messages = opening();
        messages.push(serde_json::to_value(&open).unwrap());
        let (code, dry) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": thinking(messages.clone()), "dry_run": true}));
        assert_eq!(code, 200, "{dry}");
        assert_eq!((&dry["from"], &dry["usage"]["fed"], &dry["dry_run"]), (&json!(head), &json!(0), &json!(true)), "k {k}: {dry}");
        let rest = generate(&s, thinking(messages), 300 - k);
        rest.continues(&open).unwrap();
        assert_eq!((&rest.reasoning, &rest.content, rest.finish), (&whole.reasoning, &whole.content, whole.finish), "k {k}: the reply goes on as the whole reply did");
        assert_eq!((rest.usage.prompt, rest.usage.kept, rest.usage.fed), (cut.usage.prompt + k, cut.usage.prompt + k, 0), "k {k}: the open text is prompt, kept from the head");
        assert_eq!(rest.usage.completion + k, whole.usage.completion, "k {k}: completion counts only the sampled tokens");
        assert_eq!(rest.usage.reasoning + cut.usage.reasoning, whole.usage.reasoning, "k {k}");
    }
    assert!(phases.contains(&Phase::Reasoning) && phases.contains(&Phase::Content), "both phases were cut: {phases:?}");
}

#[test]
fn a_continuations_stream_sends_only_the_sampled_tokens() {
    let s = Server::start(Config::default()).unwrap();
    let mut messages = opening();
    messages.push(json!({"role": "assistant", "reasoning": "pl", "continue": true}));
    let body = json!({"model": "toy", "context": thinking(messages), "max_tokens": 40, "sample": {"temperature": 0}});
    let events = stream(&s, body);
    let Some(StreamEvent::Done(done)) = events.last() else { panic!("{events:?}") };
    let tokens: Vec<_> = events.iter().filter_map(|e| if let StreamEvent::Token(t) = e { Some(t.clone()) } else { None }).collect();
    assert_eq!(tokens.iter().map(|t| t.n).collect::<Vec<_>>(), (1..=done.usage.completion).collect::<Vec<_>>());
    assert_eq!(tokens.iter().filter(|t| t.phase == Phase::Reasoning).count() as u64, done.usage.reasoning);
    assert!(done.usage.reasoning >= 1, "the reply starts inside the open thinking: {done:?}");
    let open: gogi_proto::Message = serde_json::from_value(json!({"role": "assistant", "reasoning": "pl", "continue": true})).unwrap();
    gogi_proto::generation::check_pieces_after(&tokens, done, &open).unwrap();
}

#[test]
fn a_content_member_closes_the_thinking_even_when_empty() {
    let s = Server::start(Config::default()).unwrap();
    let mut messages = opening();
    messages.push(json!({"role": "assistant", "reasoning": "plan", "content": "", "continue": true}));
    let d = generate(&s, thinking(messages), 20);
    assert_eq!((d.reasoning.as_str(), d.usage.reasoning), ("plan", 0), "{d:?}");
    let mut written = opening();
    written.push(json!({"role": "assistant", "reasoning": "a<endthink>b", "continue": true}));
    let d = generate(&s, thinking(written), 20);
    assert!(d.reasoning.starts_with("a<endthink>b") && d.usage.reasoning >= 1, "<endthink> in open text is text: {d:?}");
    let mut content = opening();
    content.push(json!({"role": "assistant", "content": "fn ", "continue": true}));
    let d = generate(&s, json!({"thinking": false, "messages": content}), 20);
    assert!(d.content.starts_with("fn ") && d.reasoning.is_empty() && d.usage.reasoning == 0, "{d:?}");
}

#[test]
fn a_cut_reply_keeps_its_trailing_whitespace() {
    let s = Server::start(Config::default()).unwrap();
    let whole = generate(&s, json!({"thinking": false, "messages": opening()}), 300);
    let at = whole.content.find(' ').expect("the toy writes spaces") as u64 + 1;
    let cut = generate(&s, json!({"thinking": false, "messages": opening()}), at);
    assert!(cut.content.ends_with(' '), "{cut:?}");
    let mut messages = opening();
    messages.push(serde_json::to_value(cut.as_open_message().unwrap()).unwrap());
    let rest = generate(&s, json!({"thinking": false, "messages": messages}), 300 - at);
    assert_eq!((rest.content, rest.usage.fed), (whole.content, 0));
}

#[test]
fn each_misuse_of_continue_is_a_400_naming_continue() {
    let s = Server::start(Config::default()).unwrap();
    let with = |last: Value| {
        let mut m = opening();
        m.push(last);
        m
    };
    let gen_body = |context: Value| json!({"model": "toy", "context": context, "max_tokens": 4});
    let bad = [
        thinking({ let mut m = with(json!({"role": "assistant", "content": "a", "continue": true})); m.push(json!({"role": "user", "content": "v"})); m }),
        thinking(with(json!({"role": "user", "content": "u", "continue": true}))),
        thinking(with(json!({"role": "assistant", "content": "a", "tool_calls": [{"id": "c", "name": "sh", "input": {}}], "continue": true}))),
        thinking(with(json!({"role": "assistant", "reasoning": "", "continue": true}))),
        json!({"thinking": false, "messages": with(json!({"role": "assistant", "reasoning": "r", "continue": true}))}),
        thinking(with(json!({"role": "assistant", "content": "a", "continue": "yes"}))),
        thinking(with(json!({"role": "assistant", "content": "a", "continue": null}))),
    ];
    for c in bad {
        assert_eq!(refused_param(&s, "/council/v2/generations", gen_body(c.clone())), json!("continue"), "{c}");
    }
    let open = thinking(with(json!({"role": "assistant", "content": "a", "continue": true})));
    assert_eq!(refused_param(&s, "/council/v2/contexts", json!({"model": "toy", "context": open})), json!("continue"));
    assert_eq!(refused_param(&s, "/council/v2/contexts", json!({"model": "toy", "context": open, "dry_run": false})), json!("continue"));
    let (code, dry) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": open, "dry_run": true}));
    assert_eq!((code, &dry["dry_run"], &dry["snapshots"]), (200, &json!(true), &json!([])), "{dry}");
    assert_eq!(refused_param(&s, "/council/v2/reads", json!({"model": "toy", "contexts": [open], "suffixes": ["x"], "letters": ["A", "B"]})), json!("continue"));
    let (_, spec) = post(&s, "/council/v2/specs", json!({"name": "g", "instructions": "i", "input_label": "l", "questions": [{"id": "v", "type": "noul", "instructions": "ok?"}]}));
    assert_eq!(refused_param(&s, "/council/v2/decisions", json!({"model": "toy", "spec_id": spec["spec_id"], "case": "x", "contexts": [open]})), json!("continue"));
}

#[test]
fn continue_false_on_earlier_messages_changes_nothing() {
    let s = Server::start(Config::default()).unwrap();
    let plain = json!([{"role": "user", "content": "u"}, {"role": "assistant", "content": "a"}, {"role": "user", "content": "v"}]);
    let marked = json!([{"role": "user", "content": "u", "continue": false}, {"role": "assistant", "content": "a", "continue": false}, {"role": "user", "content": "v", "continue": false}]);
    let (_, a) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(plain)}));
    let (code, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(marked)}));
    assert_eq!((code, &b["head"], &b["usage"]["fed"]), (200, &a["head"], &json!(0)), "{b}");
}

// ---- context_tokens is the longest context, or a generation's prompt and reply

/// A user message whose context renders to exactly `n` tokens with thinking on.
fn user_of(n: usize) -> Value {
    json!([{"role": "user", "content": "x".repeat(n - 3)}])
}

#[test]
fn a_context_of_exactly_context_tokens_builds_and_one_more_is_too_large() {
    let s = Server::start(Config { context_tokens: 64, ..Config::default() }).unwrap();
    let (code, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(user_of(64))}));
    assert_eq!((code, &b["usage"]["prompt"]), (200, &json!(64)), "{b}");
    let (code, e) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(user_of(65))}));
    assert_eq!((code, &e["error"]["type"], &e["error"]["param"]), (413, &json!("too_large"), &json!("context_tokens")), "{e}");
    let read = |n: usize, suffix: &str| post(&s, "/council/v2/reads", json!({"model": "toy", "contexts": [ctx(user_of(n))], "suffixes": [suffix], "letters": ["A", "B"]}));
    assert_eq!(read(62, "ab").0, 200, "a read of exactly context_tokens");
    let (code, e) = read(63, "ab");
    assert_eq!((code, &e["error"]["type"], &e["error"]["param"]), (413, &json!("too_large"), &json!("context_tokens")), "{e}");
}

#[test]
fn a_prompt_with_one_token_of_room_gets_a_reply_that_fills_it() {
    let s = Server::start(Config { context_tokens: 64, ..Config::default() }).unwrap();
    // The generation prompt adds <asst> <think>: 62 + 2 = 64 is no room, 61 + 2 = 63 leaves one.
    let (code, e) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": ctx(user_of(62)), "max_tokens": 8}));
    assert_eq!((code, &e["error"]["type"], &e["error"]["prompt_tokens"]), (413, &json!("context_full"), &json!(64)), "{e}");
    let d = generate(&s, ctx(user_of(61)), 8);
    assert_eq!((d.usage.prompt, d.usage.completion, d.finish), (63, 1, gogi_proto::generation::Finish::ContextFull), "{d:?}");
    assert_eq!(d.phase, None, "a context_full reply cannot be continued");
    let d = generate(&s, ctx(user_of(51)), 40);
    assert_eq!((d.usage.prompt + d.usage.completion, d.finish), (64, gogi_proto::generation::Finish::ContextFull), "the reply fills the last token: {d:?}");
}

#[test]
fn a_call_cut_by_max_tokens_is_kept_as_written_with_no_head() {
    let s = Server::start(Config::default()).unwrap();
    let tool = json!({"name": "next", "input_schema": {"type": "object"}});
    let context = json!({"thinking": false, "tools": [tool], "messages": [{"role": "user", "content": "CALL it"}]});
    let (code, v) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": context, "max_tokens": 3, "sample": {"temperature": 0}}));
    assert_eq!(code, 200, "{v}");
    let d: GenerationDone = serde_json::from_value(v.clone()).unwrap();
    assert_eq!((d.finish, d.phase, d.head.clone()), (gogi_proto::generation::Finish::MaxTokens, Some(Phase::Call), None), "{v}");
    let [gogi_proto::generation::ReplyToolCall::Unparsed { id, raw, error, .. }] = d.tool_calls.as_slice() else { panic!("one call kept as written: {v}") };
    assert_eq!((raw.as_str(), id.is_empty()), ("ne", false));
    assert!(error.contains("endcall"), "{error}");
    assert_eq!(d.as_open_message(), None);
    let whole = generate(&s, context, 64);
    assert_eq!((whole.finish, whole.tool_calls.len()), (gogi_proto::generation::Finish::ToolUse, 1));
}

#[test]
fn a_sealed_cut_head_rounds_the_state_a_continuation_starts_from() {
    let s = Server::start(Config { seal_cut_heads: true, ..Config::default() }).unwrap();
    let whole = generate(&s, thinking(opening()), 300);
    let mut differs = 0;
    for k in 1..whole.usage.completion.min(12) {
        let cut = generate(&s, thinking(opening()), k);
        let open = cut.as_open_message().unwrap();
        let mut messages = opening();
        messages.push(serde_json::to_value(&open).unwrap());
        let rest = generate(&s, thinking(messages.clone()), 300 - k);
        rest.continues(&open).unwrap();
        assert_eq!((rest.usage.kept, rest.usage.fed), (cut.usage.prompt + k, 0), "k {k}: the continuation starts from the sealed head");
        let again = generate(&s, thinking(messages), 300 - k);
        assert_eq!((&again.reasoning, &again.content), (&rest.reasoning, &rest.content), "k {k}: it replays from the same head");
        if (&rest.reasoning, &rest.content) != (&whole.reasoning, &whole.content) {
            differs += 1;
        }
    }
    assert!(differs > 0, "the rounded state changed a continuation");
}

/// Sends raw bytes with a media type and returns the status line's code, the head, and the body.
fn exchange_bytes(s: &Server, method: &str, path: &str, mime: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
    let addr = s.url().trim_start_matches("http://").to_string();
    let mut c = TcpStream::connect(&addr).unwrap();
    write!(c, "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    c.write_all(body).unwrap();
    let mut raw = Vec::new();
    c.read_to_end(&mut raw).unwrap();
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..at].to_vec()).unwrap();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, head, raw[at + 4..].to_vec())
}

fn put_object(s: &Server, bytes: &[u8]) -> String {
    let id = gogi_proto::content_id(bytes);
    let (code, _, body) = exchange_bytes(s, "PUT", &format!("/council/v2/objects/{id}"), "audio/wav", bytes);
    assert_eq!(code, 201, "{}", String::from_utf8_lossy(&body));
    id
}

#[test]
fn head_sends_the_headers_of_an_object_and_no_body() {
    let s = Server::start(Config::default()).unwrap();
    let id = put_object(&s, &[1, 2, 3, 0, 255]);
    let (code, head, body) = exchange_bytes(&s, "HEAD", &format!("/council/v2/objects/{id}"), "audio/wav", &[]);
    assert_eq!(code, 200);
    assert!(head.to_ascii_lowercase().contains("content-length: 5") && head.contains("audio/wav"), "{head}");
    assert!(body.is_empty(), "{body:?}");
}

#[test]
fn keeps_past_keep_bytes_are_a_413_and_keep_nothing() {
    let s = Server::start(Config { keep_bytes: 300, ..Config::default() }).unwrap();
    let a = put_object(&s, &[1u8; 200]);
    let b = put_object(&s, &[2u8; 200]);
    assert_eq!(call(&s, "PUT", "/council/v2/keeps/k1", Some(json!({"objects": [a], "ttl_s": 60}))).0, 200);
    let (code, v) = call(&s, "PUT", "/council/v2/keeps/k2", Some(json!({"objects": [b], "ttl_s": 60})));
    assert!(code == 413 && v["error"]["param"] == json!("keep_bytes"), "{v}");
    assert_eq!(call(&s, "GET", "/council/v2/keeps/k2", None).0, 404);
    // Replacing an owner's own set does not count its old set twice.
    assert_eq!(call(&s, "PUT", "/council/v2/keeps/k1", Some(json!({"objects": [b], "ttl_s": 60}))).0, 200);
}

#[test]
fn keeps_expire() {
    let s = Server::start(Config::default()).unwrap();
    let a = put_object(&s, b"kept for a minute");
    assert_eq!(call(&s, "PUT", "/council/v2/keeps/k", Some(json!({"objects": [a], "ttl_s": 60}))).0, 200);
    s.advance_clock(61);
    assert_eq!(call(&s, "GET", "/council/v2/keeps/k", None).0, 404);
}

#[test]
fn a_body_past_the_limit_is_a_413_before_it_is_read() {
    let s = Server::start(Config::default()).unwrap();
    let addr = s.url().trim_start_matches("http://").to_string();
    let mut c = TcpStream::connect(&addr).unwrap();
    let id = gogi_proto::content_id(b"x");
    write!(c, "PUT /council/v2/objects/{id} HTTP/1.1\r\nHost: x\r\nContent-Type: audio/wav\r\nContent-Length: 100000000000\r\nConnection: close\r\n\r\n").unwrap();
    let mut raw = String::new();
    c.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 413") && raw.contains("object_bytes"), "{raw}");
    // The server still answers.
    assert_eq!(call(&s, "GET", "/council/v2/identity", None).0, 200);
}

#[test]
fn a_head_error_has_no_body() {
    let s = Server::start(Config::default()).unwrap();
    let (code, head, body) = exchange_bytes(&s, "HEAD", &format!("/council/v2/objects/{}", gogi_proto::content_id(b"never")), "audio/wav", &[]);
    assert_eq!(code, 404, "{head}");
    assert!(body.is_empty(), "{}", String::from_utf8_lossy(&body));
}

#[test]
fn a_damaged_object_file_drops_the_object_and_the_server_starts() {
    let dir = std::env::temp_dir().join(format!("gogi-fake-damaged-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Server::start(Config { state_dir: Some(dir.clone()), ..Config::default() }).unwrap();
    let a = put_object(&s, b"the first kept object");
    let b = put_object(&s, b"the second kept object");
    assert_eq!(call(&s, "PUT", "/council/v2/keeps/k", Some(json!({"objects": [a, b], "ttl_s": 600}))).0, 200);
    std::fs::write(dir.join("objects").join(a.trim_start_matches("blake3:")), b"damaged").unwrap();
    s.restart().unwrap();
    assert_eq!(call(&s, "GET", "/council/v2/keeps/k", None).1["objects"], json!([b]));
    let (code, _, _) = exchange_bytes(&s, "HEAD", &format!("/council/v2/objects/{a}"), "audio/wav", &[]);
    assert_eq!(code, 404);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_call_hook_reads_the_text_parts_of_the_last_message() {
    let s = Server::start(Config::default()).unwrap();
    let tools = json!([{"name": "shell", "input_schema": {"type": "object", "properties": {}}}]);
    let id = put_object(&s, b"a sound");
    let c = json!({"thinking": false, "tools": tools, "messages": [{"role": "user", "content": [{"type": "text", "text": "CALL the tool"}, {"type": "object", "id": id, "mime": "audio/wav"}]}]});
    let (code, d) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": c, "max_tokens": 64, "sample": {"temperature": 0}}));
    assert_eq!(code, 200, "{d}");
    assert_eq!(d["finish"], "tool_use", "{d}");
}

fn with_embedder() -> Server {
    let embed = FakeModel { aliases: vec!["toy-embed".into()], ..FakeModel::embedding("gogi-toy-embed-1", 0) };
    Server::start(Config { models: vec![FakeModel::ready("gogi-toy-1", 0), embed], ..Config::default() }).unwrap()
}

fn embed(s: &Server, body: Value) -> (u16, Value) {
    post(s, "/council/v2/embeddings", body)
}

fn vector(v: &Value) -> Vec<f32> {
    v["vector"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
}

#[test]
fn identity_lists_a_model_that_only_embeds() {
    let s = with_embedder();
    let (_, id) = call(&s, "GET", "/council/v2/identity", None);
    let m = &id["models"][1];
    assert_eq!(m["model"], "gogi-toy-embed-1");
    assert_eq!(m["capabilities"], json!(["embed", "dry_run"]));
    assert_eq!(m["media"], json!({"inputs": [], "outputs": [], "text_replies": false}));
    assert_eq!(m["embeddings"], json!({"tasks": ["query", "document", "similarity"], "dimensions": [64, 32, 16], "inputs_per_request": 32}));
    assert_eq!((m["limits"]["context_tokens"].as_u64(), m["limits"]["decision_context_tokens"].as_u64()), (Some(512), Some(0)));
    assert!(!id["models"][0]["capabilities"].as_array().unwrap().contains(&json!("embed")));
}

#[test]
fn an_embedding_is_a_unit_vector_per_input_that_replays_alone() {
    let s = with_embedder();
    let (code, a) = embed(&s, json!({"model": "toy-embed", "task": "query", "inputs": ["pin a snapshot", "evict"]}));
    assert_eq!(code, 200, "{a}");
    assert_eq!((a["model"].as_str(), a["dimensions"].as_u64(), a["task"].as_str()), (Some("gogi-toy-embed-1"), Some(64), Some("query")));
    let e = a["embeddings"].as_array().unwrap();
    assert_eq!((e.len(), e[0]["index"].as_u64(), e[1]["index"].as_u64()), (2, Some(0), Some(1)));
    assert_eq!(e[1]["tokens"].as_u64(), Some(1 + "task: search result | query: ".len() as u64 + 5));
    let n: f64 = vector(&e[0]).iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    assert!((n - 1.0).abs() < 1e-6, "{n}");
    assert_eq!(a["usage"], json!({"prompt": e[0]["tokens"].as_u64().unwrap() + e[1]["tokens"].as_u64().unwrap(), "kept": 0, "fed": e[0]["tokens"].as_u64().unwrap() + e[1]["tokens"].as_u64().unwrap(), "completion": 0, "reasoning": 0}));
    assert_eq!(a["identity"]["template"], "gogi-toy-embed-1");
    let (_, alone) = embed(&s, json!({"model": "gogi-toy-embed-1", "task": "query", "inputs": ["evict"]}));
    assert_eq!(alone["embeddings"][0]["vector"], e[1]["vector"], "a vector does not depend on the other inputs");
    let (_, doc) = embed(&s, json!({"model": "gogi-toy-embed-1", "task": "document", "inputs": ["evict"]}));
    assert_ne!(doc["embeddings"][0]["vector"], e[1]["vector"], "the task changes the vector");
}

#[test]
fn a_cut_is_the_whole_vector_cut_and_normalized_again() {
    let s = with_embedder();
    let (_, whole) = embed(&s, json!({"model": "toy-embed", "task": "document", "inputs": ["a chunk of a file"]}));
    let (code, cut) = embed(&s, json!({"model": "toy-embed", "task": "document", "inputs": ["a chunk of a file"], "dimensions": 16}));
    assert_eq!((code, cut["dimensions"].as_u64()), (200, Some(16)), "{cut}");
    let want = gogi_proto::embeddings::cut(&vector(&whole["embeddings"][0]), 16);
    let got = vector(&cut["embeddings"][0]);
    assert!(got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-6), "{got:?} {want:?}");
}

#[test]
fn a_dry_run_counts_tokens_past_the_limit_and_a_real_run_refuses_them() {
    let s = with_embedder();
    let long = "x".repeat(600);
    let (code, dry) = embed(&s, json!({"model": "toy-embed", "task": "query", "inputs": ["short", long], "dry_run": true}));
    assert_eq!(code, 200, "{dry}");
    let e = &dry["embeddings"];
    assert!(e[0].get("vector").is_none() && e[1].get("vector").is_none(), "{dry}");
    assert_eq!(e[1]["tokens"].as_u64(), Some(1 + "task: search result | query: ".len() as u64 + 600));
    assert_eq!(dry["usage"]["fed"], 0);
    let (code, v) = embed(&s, json!({"model": "toy-embed", "task": "query", "inputs": ["short", long]}));
    assert_eq!((code, v["error"]["type"].as_str(), v["error"]["param"].as_str()), (413, Some("too_large"), Some("context_tokens")), "{v}");
    assert_eq!((v["error"]["prompt_tokens"].as_u64(), v["error"]["context_tokens"].as_u64()), (e[1]["tokens"].as_u64(), Some(512)));
    assert!(v["error"]["message"].as_str().unwrap().contains("inputs[1]"), "{v}");
}

#[test]
fn embedding_refusals_name_the_member() {
    let s = with_embedder();
    let id = format!("blake3:{}", "a".repeat(64));
    for (body, code, param) in [
        (json!({"task": "query", "inputs": ["a"]}), 400, "model"),
        (json!({"model": "gogi-toy-1", "task": "query", "inputs": ["a"]}), 400, "model"),
        (json!({"model": "toy-embed", "task": "rerank", "inputs": ["a"]}), 400, "task"),
        (json!({"model": "toy-embed", "task": "query", "inputs": []}), 400, "inputs"),
        (json!({"model": "toy-embed", "task": "query", "inputs": vec!["a"; 33]}), 400, "inputs"),
        (json!({"model": "toy-embed", "task": "query", "inputs": ["a", ""]}), 400, "inputs[1]"),
        (json!({"model": "toy-embed", "task": "query", "inputs": ["a"], "dimensions": 48}), 400, "dimensions"),
        (json!({"model": "toy-embed", "task": "query", "inputs": [[{"type": "object", "id": id, "mime": "image/png"}]]}), 400, "inputs[0][0]"),
        (json!({"model": "toy-embed", "task": "query", "inputs": ["a"], "normalize": false}), 400, "normalize"),
    ] {
        let (got, v) = embed(&s, body.clone());
        assert_eq!((got, v["error"]["param"].as_str()), (code, Some(param)), "{body} -> {v}");
    }
    s.set_status("gogi-toy-embed-1", ModelStatus::Loading).unwrap();
    let (got, v) = embed(&s, json!({"model": "toy-embed", "task": "query", "inputs": ["a"]}));
    assert_eq!((got, v["error"]["type"].as_str()), (503, Some("model_unavailable")), "{v}");
}

#[test]
fn a_model_that_only_embeds_refuses_contexts_naming_model() {
    let s = with_embedder();
    let c = ctx(json!([{"role": "user", "content": "u"}]));
    for (path, body) in [
        ("/council/v2/contexts", json!({"model": "toy-embed", "context": c})),
        ("/council/v2/reads", json!({"model": "toy-embed", "contexts": [c], "suffixes": ["A"], "letters": ["A", "B"]})),
        ("/council/v2/generations", json!({"model": "toy-embed", "context": c, "max_tokens": 4})),
        ("/council/v2/decisions", json!({"model": "toy-embed", "spec_id": format!("blake3:{}", "0".repeat(64)), "case": "x", "contexts": [c]})),
    ] {
        let (code, v) = post(&s, path, body);
        assert_eq!((code, v["error"]["param"].as_str()), (400, Some("model")), "{path}: {v}");
    }
}

/// A served model without thinking: the toy's numbers, and `thinking` absent from its capabilities.
fn plain() -> Server {
    Server::start(Config { models: vec![FakeModel::ready("gogi-toy-1", 0), FakeModel::without_thinking("gogi-toy-plain", 3)], ..Config::default() }).unwrap()
}

#[test]
fn every_request_that_reads_a_context_names_thinking_and_a_plain_model_takes_only_false() {
    let s = plain();
    let (_, id) = call(&s, "GET", "/council/v2/identity", None);
    let caps = |i: usize| id["models"][i]["capabilities"].as_array().unwrap().contains(&json!("thinking"));
    assert_eq!((caps(0), caps(1)), (true, false), "{id}");
    let (_, spec) = post(&s, "/council/v2/specs", json!({"name": "g", "instructions": "i", "input_label": "l", "questions": [{"id": "v", "type": "noul", "instructions": "ok?"}]}));
    let messages = json!([{"role": "user", "content": "u", "snap": true}]);
    let bodies = |model: &str, c: Value| {
        [
            ("/council/v2/contexts", json!({"model": model, "context": c}), "thinking"),
            ("/council/v2/generations", json!({"model": model, "context": c, "max_tokens": 4}), "thinking"),
            ("/council/v2/reads", json!({"model": model, "contexts": [c], "suffixes": ["x"], "letters": ["A", "B"]}), "contexts[0].thinking"),
            ("/council/v2/decisions", json!({"model": model, "spec_id": spec["spec_id"], "case": "x", "contexts": [c]}), "contexts[0].thinking"),
        ]
    };
    for model in ["gogi-toy-1", "gogi-toy-plain"] {
        for (path, body, _) in bodies(model, json!({"messages": messages})) {
            assert_eq!(refused_param(&s, path, body), json!("thinking"), "{path}: a context without thinking names it");
        }
        for (path, body, _) in bodies(model, json!({"thinking": false, "messages": messages})) {
            let (code, v) = post(&s, path, body);
            assert_eq!(code, 200, "{path} {model}: {v}");
        }
    }
    for (path, body, param) in bodies("gogi-toy-plain", json!({"thinking": true, "messages": messages})) {
        assert_eq!(refused_param(&s, path, body), json!(param), "{path}");
    }
    for (path, body, _) in bodies("gogi-toy-1", json!({"thinking": true, "messages": messages})) {
        assert_eq!(post(&s, path, body).0, 200, "{path}");
    }
}

// ---- control vectors

const CV: &str = "/council/v2/control-vectors";

/// A served model that declares control_vectors, beside one that does not.
fn steered_and_plain() -> Server {
    Server::start(Config { models: vec![FakeModel { aliases: vec!["toy".into()], ..FakeModel::ready("gogi-toy-1", 0) }, FakeModel::without_control_vectors("gogi-toy-plain", 3)], ..Config::default() }).unwrap()
}

fn said(text: &str) -> Value {
    json!({"thinking": false, "messages": [{"role": "user", "content": "Write the scene."}, {"role": "assistant", "content": text}]})
}

/// Makes a control vector on gogi-toy-1 and returns the response.
fn make_vector(s: &Server) -> Value {
    let (code, r) = post(s, CV, json!({"model": "toy", "positive": [said("I can't write that."), said("No.")], "negative": [said("The dragon lands.")]}));
    assert_eq!(code, 200, "{r}");
    r
}

fn put_typed(s: &Server, bytes: &[u8], mime: &str) -> String {
    let id = gogi_proto::content_id(bytes);
    let (code, _, body) = exchange_bytes(s, "PUT", &format!("/council/v2/objects/{id}"), mime, bytes);
    assert!(code == 201 || code == 200, "{}", String::from_utf8_lossy(&body));
    id
}

fn steer(vector: &str, op: &str, strength: f64) -> Value {
    json!({"vector": vector, "op": op, "strength": strength})
}

fn with_control(control: Value) -> Value {
    json!({"thinking": false, "control": control, "messages": [{"role": "user", "content": "Write the scene where the dragon wins.", "snap": true}]})
}

/// The letter log probabilities of a raw read after `context`.
fn letters_after(s: &Server, context: Value) -> Value {
    let (code, r) = post(s, "/council/v2/reads", json!({"model": "toy", "contexts": [context], "suffixes": ["Answer: "], "letters": ["A", "B", "C"]}));
    assert_eq!(code, 200, "{r}");
    r["reads"][0]["logprobs"].clone()
}

#[test]
fn a_model_declares_control_vectors_with_its_limit_unless_built_without() {
    let s = steered_and_plain();
    let (_, id) = call(&s, "GET", "/council/v2/identity", None);
    let caps = |i: usize| id["models"][i]["capabilities"].as_array().unwrap().contains(&json!("control_vectors"));
    assert!(caps(0) && !caps(1), "{id}");
    assert_eq!(id["models"][0]["limits"]["control_vector_contexts"], json!(4));
    assert!(id["models"][1]["limits"].get("control_vector_contexts").is_none());
}

#[test]
fn the_route_puts_the_difference_of_the_mean_residuals_as_an_object() {
    use gogi_toy::control::{LAYERS, WIDTH, residuals};
    let s = steered_and_plain();
    let r = make_vector(&s);
    assert_eq!((r["model"].clone(), r["layers"].clone(), r["width"].clone(), r["contexts"].clone()), (json!("gogi-toy-1"), json!(LAYERS), json!(WIDTH), json!({"positive": 2, "negative": 1})));
    assert_eq!(r["identity"]["weight_hash"], json!("0x0000000000000000"));
    let id = r["vector"].as_str().unwrap().to_string();
    let (code, head, bytes) = exchange_bytes(&s, "GET", &format!("/council/v2/objects/{id}"), "x/y", b"");
    assert_eq!(code, 200);
    assert!(head.to_ascii_lowercase().contains("content-type: application/x-safetensors"), "{head}");
    assert_eq!(gogi_proto::content_id(&bytes), id);
    let v = gogi_proto::control::ControlVector::parse(&bytes).unwrap();
    assert_eq!((v.model.as_str(), v.weight_hash.as_str(), v.layers.len(), v.width()), ("gogi-toy-1", "0x0000000000000000", LAYERS as usize, WIDTH));
    // The residual after each layer at the last prompt token: the prompt is the rendering with the generation prompt.
    let at = |c: Value| {
        let c: gogi_proto::Context = serde_json::from_value(c).unwrap();
        let p = gogi_toy::render_tokens(&c.tools, c.thinking, &c.messages, true);
        residuals(gogi_toy::State::after(0, &p))
    };
    let (p0, p1, n0) = (at(said("I can't write that.")), at(said("No.")), at(said("The dragon lands.")));
    for l in 0..LAYERS as usize {
        for j in 0..WIDTH {
            let want = ((p0[l][j] + p1[l][j]) / 2.0 - n0[l][j]) as f32;
            assert_eq!(v.layers[&(l as u32)][j], want, "layer {l}, component {j}");
        }
    }
    assert_eq!(make_vector(&s)["vector"], r["vector"], "the same request puts the same bytes");
    let u: gogi_proto::Usage = serde_json::from_value(r["usage"].clone()).unwrap();
    u.check().unwrap();
    let media = u.media.unwrap();
    assert_eq!((media.written.objects, media.written.bytes, u.completion), (1, bytes.len() as u64, 0));
}

#[test]
fn a_dry_run_of_the_route_puts_nothing_and_its_refusals_name_the_member() {
    let s = steered_and_plain();
    let (code, d) = post(&s, CV, json!({"model": "toy", "positive": [said("No.")], "negative": [said("Yes.")], "dry_run": true}));
    assert_eq!((code, &d["vector"], &d["width"]), (200, &Value::Null, &json!(8)), "{d}");
    let made = make_vector(&s);
    assert_ne!(made["vector"], Value::Null);
    let steered = json!({"thinking": false, "control": [steer(made["vector"].as_str().unwrap(), "add", 1.0)], "messages": [{"role": "user", "content": "u"}]});
    for (body, param) in [
        (json!({"model": "toy", "positive": [], "negative": [said("Yes.")]}), "positive"),
        (json!({"model": "toy", "positive": [said("No.")], "negative": [said("Yes."), steered]}), "negative[1].control"),
        (json!({"model": "toy", "positive": vec![said("No."); 5], "negative": [said("Yes.")]}), "positive"),
        (json!({"model": "gogi-toy-plain", "positive": [said("No.")], "negative": [said("Yes.")]}), "model"),
        (json!({"model": "toy", "positive": [said("No.")], "negative": [said("Yes.")], "normalize": true}), "normalize"),
    ] {
        assert_eq!(refused_param(&s, CV, body.clone()), json!(param), "{body}");
    }
}

#[test]
fn add_and_ablate_at_0_give_the_plain_numbers_and_clamp_and_add_change_them() {
    let s = steered_and_plain();
    let v = make_vector(&s)["vector"].as_str().unwrap().to_string();
    let plain = letters_after(&s, with_control(json!([])));
    assert_eq!(letters_after(&s, with_control(json!([steer(&v, "add", 0.0), steer(&v, "ablate", 0.0)]))), plain);
    assert_ne!(letters_after(&s, with_control(json!([steer(&v, "clamp", 0.0)]))), plain, "clamp at 0 ablates");
    assert_ne!(letters_after(&s, with_control(json!([steer(&v, "add", 4.0)]))), plain);
    assert_ne!(letters_after(&s, with_control(json!([steer(&v, "ablate", 1.0)]))), plain);
    let order = |a: &str, b: &str| letters_after(&s, with_control(json!([steer(&v, a, 2.0), steer(&v, b, 0.5)])));
    let clamped = letters_after(&s, with_control(json!([steer(&v, "clamp", 0.5)])));
    assert_eq!(order("add", "clamp"), clamped, "a clamp after an add sets the part along the vector whatever the add did");
    assert_ne!(order("clamp", "add"), clamped, "the items apply in list order");
    let reply = |control: Value| generate(&s, with_control(control), 24).content;
    assert_eq!(reply(json!([steer(&v, "add", 0.0)])), reply(json!([])), "a generation at strength 0 writes the plain reply");
}

#[test]
fn a_control_list_has_model_state_of_its_own() {
    let s = steered_and_plain();
    let v = make_vector(&s)["vector"].as_str().unwrap().to_string();
    let build = |c: Value| post(&s, "/council/v2/contexts", json!({"model": "toy", "context": c})).1;
    let plain = build(with_control(json!([])));
    let zero = build(with_control(json!([steer(&v, "add", 0.0)])));
    assert_ne!(zero["head"], plain["head"], "a list that changes nothing still has its own snapshots");
    assert_eq!(zero["from"], Value::Null, "{zero}");
    let again = build(with_control(json!([steer(&v, "add", 0.0)])));
    assert_eq!((&again["head"], again["usage"]["fed"].as_u64()), (&zero["head"], Some(0)), "an equal list shares its model state");
    let other = build(with_control(json!([steer(&v, "add", 0.5)])));
    assert!(other["head"] != zero["head"] && other["from"] == Value::Null, "{other}");
}

#[test]
fn each_control_refusal_names_its_member() {
    let s = steered_and_plain();
    let v = make_vector(&s)["vector"].as_str().unwrap().to_string();
    let spec = post(&s, "/council/v2/specs", json!({"name": "s", "instructions": "i", "input_label": "x", "questions": [{"id": "q", "type": "noul", "instructions": "yes?"}]})).1;
    let bodies = |model: &str, c: Value| {
        vec![
            ("/council/v2/contexts", json!({"model": model, "context": c}), ""),
            ("/council/v2/generations", json!({"model": model, "context": c, "max_tokens": 4}), ""),
            ("/council/v2/reads", json!({"model": model, "contexts": [c], "suffixes": ["x"], "letters": ["A", "B"]}), "contexts[0]."),
            ("/council/v2/decisions", json!({"model": model, "spec_id": spec["spec_id"], "case": "x", "contexts": [c]}), "contexts[0]."),
        ]
    };
    for (path, body, under) in bodies("gogi-toy-plain", with_control(json!([steer(&v, "add", 0.0)]))) {
        let e = refused_param(&s, path, body);
        assert_eq!(e, json!(format!("{under}control")), "{path}");
    }
    for (path, body, under) in bodies("toy", with_control(json!([steer(&v, "add", 1.0), steer(&v, "boost", 1.0)]))) {
        assert_eq!(refused_param(&s, path, body), json!(format!("{under}control[1].op")), "{path}");
    }
    let unheld = format!("blake3:{}", "e".repeat(64));
    for (path, body, under) in bodies("toy", with_control(json!([steer(&unheld, "add", 1.0)]))) {
        let (code, e) = post(&s, path, body);
        assert_eq!((code, &e["error"]["param"]), (404, &json!(format!("{under}control[0].vector"))), "{path}: {e}");
        assert!(e["error"]["message"].as_str().unwrap().contains(&unheld));
    }
    let id = r#"{"model": "toy"}"#.as_bytes();
    let wav = put_typed(&s, id, "audio/wav");
    let junk = put_typed(&s, b"not a safetensors file", "application/x-safetensors");
    let theirs = gogi_proto::control::ControlVector { model: "gogi-toy-plain".into(), weight_hash: "0x0000000000000003".into(), layers: [(0, vec![1.0; 8])].into_iter().collect() };
    let wide = gogi_proto::control::ControlVector { model: "gogi-toy-1".into(), weight_hash: "0x0000000000000000".into(), layers: [(0, vec![1.0; 9])].into_iter().collect() };
    let deep = gogi_proto::control::ControlVector { model: "gogi-toy-1".into(), weight_hash: "0x0000000000000000".into(), layers: [(4, vec![1.0; 8])].into_iter().collect() };
    for (vector, says) in [
        (wav, "audio/wav"),
        (junk, "not a control vector"),
        (put_typed(&s, &theirs.to_bytes(), "application/x-safetensors"), "weight_hash"),
        (put_typed(&s, &wide.to_bytes(), "application/x-safetensors"), "width"),
        (put_typed(&s, &deep.to_bytes(), "application/x-safetensors"), "layers.4"),
    ] {
        let (code, e) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": with_control(json!([steer(&v, "add", 1.0), steer(&vector, "add", 1.0)])), "max_tokens": 4}));
        assert_eq!((code, &e["error"]["param"]), (400, &json!("control[1].vector")), "{says}: {e}");
        assert!(e["error"]["message"].as_str().unwrap().contains(says), "{says}: {e}");
    }
}

#[test]
fn usage_counts_a_control_vector_read() {
    let s = steered_and_plain();
    let v = make_vector(&s)["vector"].as_str().unwrap().to_string();
    let (_, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": with_control(json!([steer(&v, "add", 1.0), steer(&v, "clamp", 1.0)]))}));
    assert_eq!(b["usage"]["media"]["read"]["objects"], json!(1), "{b}");
}

fn record_of(s: &Server, text: &str) -> String {
    let (code, b) = post(s, "/council/v2/contexts", json!({"model": "toy", "context": ctx(json!([{"role": "user", "content": text}]))}));
    assert_eq!(code, 200, "{b}");
    b["context"].as_str().unwrap().to_string()
}

fn held(s: &Server, record: &str) -> bool {
    call(s, "GET", &format!("/council/v2/contexts/{record}"), None).0 == 200
}

#[test]
fn records_past_the_limit_go_the_least_recently_named_first_and_a_pin_keeps_one() {
    let s = Server::start(Config { context_records: 3, ..Config::default() }).unwrap();
    let (a, b, c) = (record_of(&s, "a"), record_of(&s, "b"), record_of(&s, "c"));
    // A pin keeps `a`. A request that names `b` as its parent makes `b` the most recently named,
    // and it holds a fourth record.
    let (code, p) = call(&s, "PUT", "/council/v2/pins/o", Some(json!({"snapshots": [], "contexts": [a], "ttl_s": 60})));
    assert_eq!((code, p["contexts"][0]["held"].clone(), p["expires_in_s"].clone()), (200, json!(true), json!(60)));
    let (code, d) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": {"parent": b, "messages": [{"role": "user", "content": "d"}]}}));
    assert_eq!(code, 200, "{d}");
    assert_eq!((held(&s, &a), held(&s, &b), held(&s, &c), held(&s, d["context"].as_str().unwrap())), (true, true, false, true));
    // The record that went is a 404 for a request that names it, and the context sent whole holds it again.
    let (code, e) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": {"parent": c}}));
    assert_eq!((code, e["error"]["type"].clone(), e["error"]["param"].clone()), (404, json!("not_found"), json!("context.parent")));
    assert_eq!(record_of(&s, "c"), c);
    assert!(held(&s, &a), "the pinned record stays");
    // When the pin runs out, the record can go.
    s.advance_clock(61);
    for text in ["e", "f", "g"] {
        record_of(&s, text);
    }
    assert!(!held(&s, &a));
}

#[test]
fn a_stream_names_the_record_of_its_reply_and_the_record_serves_the_next_request() {
    let s = Server::start(Config::default()).unwrap();
    // With thinking off, the reply has no thinking for the template to drop, so the next request starts
    // from the head after the reply.
    let context = json!({"thinking": false, "messages": [{"role": "user", "content": "hello"}]});
    let events = stream(&s, json!({"model": "toy", "context": context, "max_tokens": 64, "sample": {"temperature": 0}}));
    let Some(StreamEvent::Done(done)) = events.last() else { panic!("no done") };
    let record = done.context.clone().expect("done names a record").to_string();
    let (_, r) = call(&s, "GET", &format!("/council/v2/contexts/{record}"), None);
    let messages = r["context"]["messages"].as_array().unwrap();
    assert_eq!((messages.len(), messages[1]["role"].clone(), messages[1]["snap"].clone()), (2, json!("assistant"), json!(true)));
    // The record of the request's own context is held too.
    let own = gogi_proto::context_id(&serde_json::from_value(context).unwrap()).to_string();
    assert!(held(&s, &own));
    let (code, next) = post(&s, "/council/v2/generations", json!({"model": "toy", "max_tokens": 8, "sample": {"temperature": 0}, "context": {"parent": record, "messages": [{"role": "user", "content": "more"}]}}));
    assert_eq!(code, 200, "{next}");
    assert!(next["usage"]["kept"].as_u64().unwrap() >= done.usage.prompt, "{}", next["usage"]);
}

#[test]
fn a_request_that_fails_its_checks_holds_no_record() {
    let s = Server::start(Config::default()).unwrap();
    let context = ctx(json!([{"role": "user", "content": "no room"}]));
    let (code, _) = post(&s, "/council/v2/generations", json!({"model": "toy", "context": context, "max_tokens": 0}));
    assert_eq!(code, 400);
    let own = gogi_proto::context_id(&serde_json::from_value(context).unwrap()).to_string();
    assert!(!held(&s, &own));
}

#[test]
fn records_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("gogi-fake-records-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Server::start(Config { state_dir: Some(dir.clone()), ..Config::default() }).unwrap();
    let a = record_of(&s, "keep my text");
    s.restart().unwrap();
    assert!(held(&s, &a));
    let (code, b) = post(&s, "/council/v2/contexts", json!({"model": "toy", "context": {"parent": a}}));
    assert_eq!((code, b["context"].clone()), (200, json!(a)));
    let _ = std::fs::remove_dir_all(dir);
}

// ---- the head of a plain turn

fn one_unmarked(text: &str) -> Vec<Value> {
    vec![json!({"role": "user", "content": text})]
}

/// A plain turn with no message marked, then a second turn by `parent` with one unmarked message.
fn plain_turns(s: &Server, text: &str) -> (GenerationDone, GenerationDone) {
    let first = generate(s, json!({"thinking": false, "messages": one_unmarked(text)}), 300);
    let second = generate(s, json!({"thinking": false, "parent": first.context.as_ref().unwrap().to_string(), "messages": [{"role": "user", "content": "And again?"}]}), 300);
    (first, second)
}

#[test]
fn a_reply_to_an_unmarked_message_holds_a_head_and_the_next_turn_feeds_only_its_new_message() {
    let s = Server::start(Config::default()).unwrap();
    let (first, second) = plain_turns(&s, "Say something plain.");
    assert!(first.head.is_some(), "{first:?}");
    assert!(first.usage.completion > 8, "the test needs a reply longer than the slack: {first:?}");
    assert!(second.usage.kept + 8 >= first.usage.prompt + first.usage.completion, "{:?} {:?}", first.usage, second.usage);
}

#[test]
fn a_fake_that_loses_the_head_of_an_unmarked_reply_feeds_the_reply_again() {
    let s = Server::start(Config { lose_plain_heads: true, ..Config::default() }).unwrap();
    let (first, second) = plain_turns(&s, "Say something plain.");
    assert_eq!(first.head, None, "{first:?}");
    assert!(second.usage.kept + 8 < first.usage.prompt + first.usage.completion, "{:?} {:?}", first.usage, second.usage);
}

#[test]
fn a_fake_that_loses_the_head_of_an_unmarked_reply_keeps_the_head_of_a_marked_one() {
    let s = Server::start(Config { lose_plain_heads: true, ..Config::default() }).unwrap();
    let d = generate(&s, json!({"thinking": false, "messages": [{"role": "user", "content": "Marked.", "snap": true}]}), 300);
    assert!(d.head.is_some(), "{d:?}");
}
