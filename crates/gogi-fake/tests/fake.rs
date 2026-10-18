use std::io::{Read, Write};
use std::net::TcpStream;

use gogi_fake::{Config, Server};
use gogi_proto::decision::DecisionResponse;
use serde_json::{Value, json};

fn call(s: &Server, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let addr = s.url().trim_start_matches("http://").to_string();
    let mut c = TcpStream::connect(&addr).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    write!(c, "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).unwrap();
    let mut raw = String::new();
    c.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, serde_json::from_str(body).unwrap_or(Value::Null))
}

fn post(s: &Server, path: &str, body: Value) -> (u16, Value) {
    call(s, "POST", path, Some(body))
}

fn ctx(messages: Value) -> Value {
    json!({"messages": messages})
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
    let (_, a) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m))}));
    assert_eq!((a["kept"].as_u64(), a["from"].clone()), (Some(0), Value::Null));
    let (_, b) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m))}));
    assert_eq!((b["fed"].as_u64(), &b["head"]), (Some(0), &a["head"]));
    m[2] = json!({"role": "assistant", "content": "a", "reasoning": "r", "snap": true, "tool_calls": [{"id": "c", "name": "sh", "arguments": {}}]});
    m.push(json!({"role": "tool", "tool_call_id": "c", "content": "out"}));
    let (st, c) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m))}));
    assert_eq!(st, 200, "{c}");
    assert!(c["kept"].as_u64().unwrap() > 0 && c["fed"].as_u64().unwrap() < c["tokens"].as_u64().unwrap(), "{c}");
}

#[test]
fn a_text_answers_boundary_goes_when_a_user_message_follows_and_a_call_keeps_its_own() {
    let s = Server::start(Config::default()).unwrap();
    let mut m = answer_with_reasoning();
    let (_, a) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m))}));
    assert_eq!(a["snapshots"].as_array().unwrap().iter().filter(|x| x["message"] == 2).count(), 1);
    m.push(json!({"role": "user", "content": "next", "snap": true}));
    let (_, b) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m))}));
    let msgs: Vec<u64> = b["snapshots"].as_array().unwrap().iter().map(|x| x["message"].as_u64().unwrap()).collect();
    assert_eq!(msgs, [1, 3], "message 2 lost its boundary: {b}");
    // The user boundary is still reused.
    assert_eq!(b["from"], a["snapshots"][0]["id"]);

    let mut calls = answer_with_reasoning();
    calls[2] = json!({"role": "assistant", "reasoning": "r", "snap": true, "tool_calls": [{"id": "c", "name": "sh", "arguments": {}}]});
    calls.push(json!({"role": "tool", "tool_call_id": "c", "content": "out"}));
    calls.push(json!({"role": "user", "content": "next", "snap": true}));
    let (_, c) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(calls))}));
    assert!(c["snapshots"].as_array().unwrap().iter().any(|x| x["message"] == 2), "{c}");
}

#[test]
fn a_dry_run_builds_nothing() {
    let s = Server::start(Config::default()).unwrap();
    let m = answer_with_reasoning();
    let (_, d) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(m)), "dry_run": true}));
    assert_eq!((d["dry_run"].clone(), d["snapshots"].clone(), d["kept"].as_u64()), (json!(true), json!([]), Some(0)));
    let (code, _) = call(&s, "GET", &format!("/council/v2/snapshots/{}", d["head"].as_str().unwrap()), None);
    assert_eq!(code, 404);
}

#[test]
fn a_generation_replays_bit_for_bit_and_its_reply_extends_into_the_held_head() {
    let s = Server::start(Config::default()).unwrap();
    let body = json!({"context": ctx(json!([{"role": "user", "content": "hi", "snap": true}])), "max_tokens": 40, "sample": {"temperature": 0}});
    let (_, a) = post(&s, "/council/v2/generations", body.clone());
    let (_, b) = post(&s, "/council/v2/generations", body);
    assert_eq!((a["content"].clone(), a["reasoning"].clone(), a["head"].clone()), (b["content"].clone(), b["reasoning"].clone(), b["head"].clone()));
    assert_ne!(a["head"], Value::Null);
    // Sending the reply back lands on the head the generation reported.
    let mut reply = json!({"role": "assistant", "content": a["content"]});
    if a["reasoning"] != json!("") {
        reply["reasoning"] = a["reasoning"].clone();
    }
    let (_, built) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!([{"role": "user", "content": "hi", "snap": true}, reply]))}));
    assert_eq!(built["head"], a["head"]);
    assert_eq!(built["fed"], 0);
}

#[test]
fn a_stream_sends_token_events_then_done_with_the_same_reply() {
    let s = Server::start(Config::default()).unwrap();
    let mut body = json!({"context": ctx(json!([{"role": "user", "content": "hi"}])), "max_tokens": 12, "sample": {"temperature": 0}});
    let (_, unstreamed) = post(&s, "/council/v2/generations", body.clone());
    body["stream"] = json!(true);
    let addr = s.url().trim_start_matches("http://").to_string();
    let mut c = TcpStream::connect(addr).unwrap();
    let b = body.to_string();
    write!(c, "POST /council/v2/generations HTTP/1.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).unwrap();
    let mut raw = String::new();
    c.read_to_string(&mut raw).unwrap();
    let events: Vec<&str> = raw.lines().filter_map(|l| l.strip_prefix("event: ")).collect();
    assert_eq!(*events.last().unwrap(), "done");
    assert_eq!(events.iter().filter(|e| **e == "token").count() as u64, unstreamed["usage"]["completion"].as_u64().unwrap());
    let done: Value = serde_json::from_str(raw.lines().rev().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    assert_eq!(done["content"], unstreamed["content"]);
}

#[test]
fn a_prompt_with_no_room_is_a_413_context_full_and_a_bad_context_a_400() {
    let s = Server::start(Config { context_tokens: 64, ..Config::default() }).unwrap();
    let long = json!([{"role": "user", "content": "x".repeat(200)}]);
    for (path, body) in [("/council/v2/contexts", json!({"context": ctx(long.clone())})), ("/council/v2/generations", json!({"context": ctx(long)}))] {
        let (code, e) = post(&s, path, body);
        assert_eq!((code, e["error"]["type"].clone()), (413, json!("context_full")), "{path}");
        assert!(e["error"]["prompt_tokens"].as_u64().unwrap() > e["error"]["context_tokens"].as_u64().unwrap());
    }
    let (code, _) = post(&s, "/council/v2/generations", json!({"context": ctx(json!([{"role": "tool", "tool_call_id": "nope", "content": "x"}]))}));
    assert_eq!(code, 400);
    let (code, _) = call(&s, "PUT", "/council/v2/contexts/old", Some(json!({})));
    assert_eq!(code, 404);
}

#[test]
fn pins_name_their_snapshot_not_its_chain_expire_and_report_unheld_ids() {
    let s = Server::start(Config::default()).unwrap();
    let (_, b) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(answer_with_reasoning()))}));
    let (first, head) = (b["snapshots"][0]["id"].as_str().unwrap(), b["head"].as_str().unwrap());
    let unheld = format!("snap:{}", "0".repeat(64));
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
    let (_, b) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!(answer_with_reasoning()))}));
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
    let (_, pinned) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!([{"role": "user", "content": "pin me"}]))}));
    let (_, loose) = post(&s, "/council/v2/contexts", json!({"context": ctx(json!([{"role": "user", "content": "forget me"}]))}));
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
    let req = json!({"spec_id": a["spec_id"], "state": "rm -rf build", "pool": {"method": "loglinear", "weights": "mass"},
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
    assert_eq!(post(&s, "/council/v2/decisions", json!({"spec_id": format!("sha256:{}", "a".repeat(64)), "state": "x"})).0, 404);
    assert_eq!(post(&s, "/council/v2/decisions", json!({"spec_id": a["spec_id"], "state": "x", "contexts": [{"id": "old"}]})).0, 400);
    assert_eq!(call(&s, "DELETE", &format!("/council/v2/specs/{}", a["spec_id"].as_str().unwrap()), None).0, 204);
}

#[test]
fn a_queue_context_gets_the_case_as_the_calls_result() {
    let s = Server::start(Config::default()).unwrap();
    let spec = json!({"name": "g", "instructions": "i", "input_label": "x", "questions": [
        {"id": "verdict", "type": "choice", "instructions": "what", "criteria": [{"option": "allow", "means": "a"}, {"option": "ask", "means": "b"}]},
        {"id": "undo", "type": "score", "instructions": "how", "criteria": ["easy", "hard", "never"]}]});
    let (_, held) = post(&s, "/council/v2/specs", spec);
    let tool = |undo: &[&str]| json!({"type": "function", "function": {"name": "next", "parameters": {"type": "object", "properties": {"answers": {"type": "object", "properties": {
        "verdict": {"type": "string", "enum": ["A", "B"]}, "undo": {"type": "string", "enum": undo}}}}}}});
    let queue = |undo: &[&str]| json!({"tools": [tool(undo)], "queue": "next", "messages": [
        {"role": "system", "content": "s"}, {"role": "user", "content": "go", "snap": true},
        {"role": "assistant", "tool_calls": [{"id": "c0", "name": "next", "arguments": {}}]}]});
    let req = |c: Value| json!({"spec_id": held["spec_id"], "state": "git push", "contexts": [c]});
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
    let body = json!({"contexts": [ctx(json!([{"role": "user", "content": "u <end>", "snap": true}]))], "suffixes": ["<asst>A", "plain"], "letters": ["A", "B", "C"], "top": 3});
    let (code, a) = post(&s, "/council/v2/reads", body.clone());
    assert_eq!(code, 200, "{a}");
    let (_, b) = post(&s, "/council/v2/reads", body);
    assert_eq!(a["reads"], b["reads"]);
    assert_eq!(a["reads"].as_array().unwrap().len(), 2);
    assert_eq!(a["reads"][0]["top"].as_array().unwrap().len(), 3);
    assert_eq!(a["signals"]["control_text"], json!([{"where": "suffix:0", "token": "<asst>"}]));
    let lp: Vec<f64> = ["A", "B", "C"].iter().map(|l| a["reads"][1]["logprobs"][*l].as_f64().unwrap()).collect();
    assert!((a["reads"][1]["mass"].as_f64().unwrap() - gogi_proto::math::log_sum_exp(&lp)).abs() < 1e-12);
    let (code, _) = post(&s, "/council/v2/reads", json!({"contexts": [ctx(json!([{"role": "user", "content": "u"}]))], "suffixes": ["x"], "letters": ["AB", "C"]}));
    assert_eq!(code, 400);
}
