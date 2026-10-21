import json
import math
import pathlib
import urllib.error
import urllib.request

import pytest

import gogi

FIXTURES = pathlib.Path(__file__).resolve().parents[2] / "gogi-proto" / "tests" / "fixtures"
KAIJUTSU_FIXTURE_ID = "sha256:5114ff063b887c710333afd4fe35238c55946ac2a63fb76d6c9004b56a1503ea"


def test_spec_id_matches_kaijutsus_fixture():
    spec = json.loads((FIXTURES / "spec.json").read_text())
    assert gogi.spec_id(spec) == KAIJUTSU_FIXTURE_ID


def test_spec_id_ignores_member_order_and_refuses_fractions():
    assert gogi.spec_id({"a": 1, "b": 2}) == gogi.spec_id('{"b": 2, "a": 1}')
    assert gogi.canonical_json({"b": 1, "a": [True, None]}) == '{"a":[true,null],"b":1}'
    with pytest.raises(ValueError, match="integers only"):
        gogi.spec_id({"x": 1.5})


def test_context_rules():
    ok = {"messages": [{"role": "user", "content": "hi"}]}
    gogi.check_context(ok)
    with pytest.raises(ValueError, match="at least one message"):
        gogi.check_context({"messages": []})
    bad = {"messages": [{"role": "tool", "tool_call_id": "a", "content": "r"}]}
    with pytest.raises(ValueError, match=r"messages\[0\]: tool_call_id a names no call"):
        gogi.check_context(bad)
    with pytest.raises(ValueError, match="queue nope names no tool"):
        gogi.check_context({**ok, "queue": "nope"})


def test_continue_opens_only_a_last_assistant_message():
    u = {"role": "user", "content": "u"}
    gogi.check_context({"thinking": True, "messages": [u, {"role": "assistant", "reasoning": "First I", "continue": True}]})
    gogi.check_context({"messages": [{**u, "continue": False}, {"role": "assistant", "content": "Plan:", "continue": True}]})
    for bad in [
        [u, {"role": "assistant", "content": "a", "continue": True}, u],
        [{**u, "continue": True}],
        [u, {"role": "assistant", "content": "", "continue": True}],
        [u, {"role": "assistant", "content": "a", "tool_calls": [{"id": "c0", "name": "shell", "input": {}}], "continue": True}],
    ]:
        with pytest.raises(ValueError, match="continue: true|open message"):
            gogi.check_context({"messages": bad})
    with pytest.raises(ValueError, match="needs thinking: true"):
        gogi.check_context({"messages": [u, {"role": "assistant", "reasoning": "First I", "continue": True}]})
    with pytest.raises(ValueError, match="invalid field `continue`"):
        gogi.check_context({"messages": [u, {"role": "assistant", "content": "a", "continue": "yes"}]})


def test_tools_calls_and_results_take_the_anthropic_shape():
    tool = {"name": "shell", "description": "Runs a command.", "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}}}
    call = {"role": "assistant", "tool_calls": [{"id": "c0", "name": "shell", "input": {"command": "ls"}}]}
    failed = {"role": "tool", "tool_call_id": "c0", "content": "exit 1", "is_error": True}
    gogi.check_context({"tools": [tool], "messages": [{"role": "user", "content": "u"}, call, failed]})
    with pytest.raises(ValueError):
        gogi.check_context({"tools": [{"type": "function", "function": {"name": "shell", "parameters": {}}}], "messages": [{"role": "user", "content": "u"}]})
    with pytest.raises(ValueError):
        gogi.validate_messages([], [{"role": "assistant", "tool_calls": [{"id": "c0", "name": "shell", "arguments": {}}]}])
    with pytest.raises(ValueError, match="only a tool message carries is_error"):
        gogi.validate_messages([], [{"role": "user", "content": "u", "is_error": True}])
    with pytest.raises(ValueError, match="no other tool has"):
        gogi.check_context({"tools": [tool, tool], "messages": [{"role": "user", "content": "u"}]})


def test_parse_spec_id_refuses_text_without_describe():
    spec = json.loads((FIXTURES / "spec.json").read_text())
    with pytest.raises(ValueError, match="describe capability"):
        gogi.parse_spec_id(spec)
    assert gogi.parse_spec_id(spec, describe=True) == KAIJUTSU_FIXTURE_ID


def test_read_relations():
    r = gogi.read([-0.5, -1.5, -4.0])
    assert r["mass"] == pytest.approx(math.log(sum(math.exp(x) for x in [-0.5, -1.5, -4.0])), abs=1e-12)
    assert sum(r["probabilities"]) == pytest.approx(1.0, abs=1e-12)
    assert r["confidence"] == pytest.approx(math.exp(r["mass"]) * max(r["probabilities"]), abs=1e-12)


def test_pool_matches_the_weighted_mean_and_the_veto():
    a, b = [-0.1, -3.0, -5.0], [-2.5, -0.4, -3.5]
    p = gogi.pool([a, b], "linear", "uniform")
    ra, rb = gogi.read(a), gogi.read(b)
    for o in range(3):
        assert p["probabilities"][o] == pytest.approx((ra["probabilities"][o] + rb["probabilities"][o]) / 2, abs=1e-12)
    assert p["leave_one_out"][0] == pytest.approx(rb["probabilities"], abs=1e-12)
    veto = gogi.pool([[-0.1, -2.5, -9.0], [-9.0, -2.5, -0.1]], "loglinear")
    assert max(range(3), key=lambda i: veto["probabilities"][i]) == 1
    with pytest.raises(ValueError, match="values go with weights given"):
        gogi.pool([a], "linear", "uniform", [1.0])
    with pytest.raises(ValueError):
        gogi.pool([a, b], "linear", "given", [0.0, 0.0])


def test_verify_decision_accepts_the_fixture_and_names_a_changed_number():
    resp = json.loads((FIXTURES / "decision_response.json").read_text())
    req = json.loads((FIXTURES / "decision_request.json").read_text())
    gogi.verify_decision(req, resp)
    with pytest.raises(ValueError, match="case"):
        gogi.verify_decision({k: v for k, v in req.items() if k != "case"}, resp)
    bad_usage = json.loads(json.dumps(resp))
    bad_usage["usage"]["fed"] += 1
    with pytest.raises(ValueError, match=r"usage\.prompt"):
        gogi.verify_decision(req, bad_usage)
    resp["answers"]["verdict"]["probabilities"]["ask"] += 1e-6
    with pytest.raises(ValueError, match=r"answers\.verdict\.probabilities\.ask"):
        gogi.verify_decision(req, resp)


GATE = {"name": "gate", "instructions": "Judge the statement.", "input_label": "Statement",
        "questions": [{"id": "verdict", "type": "choice", "instructions": "What happens with it.",
                       "criteria": [{"option": "allow", "means": "routine"}, {"option": "ask", "means": "risky"}]}]}


def _call_with_id(url, method, path, body=None):
    """The status, the response, and the ``request-id`` header."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url + path, data=data, method=method, headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"null"), r.headers.get("request-id")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null"), e.headers.get("request-id")


def _call(url, method, path, body=None):
    status, body, _ = _call_with_id(url, method, path, body)
    return status, body


def test_the_fake_serves_and_its_spec_ids_match_ours():
    with gogi.FakeServer() as fake:
        status, ident = _call(fake.url, "GET", "/council/v2/identity")
        assert status == 200 and ident["models"][0]["engine"].startswith("gogi-fake")
        status, held = _call(fake.url, "POST", "/council/v2/specs", GATE)
        assert status == 200 and held["spec_id"] == gogi.spec_id(GATE)
    with pytest.raises(ValueError, match="closed"):
        fake.url


def test_the_fake_keeps_specs_and_pins_across_a_restart_and_expires_pins(tmp_path):
    ctx = {"messages": [{"role": "user", "content": "u", "snap": True}]}
    with gogi.FakeServer(state_dir=tmp_path) as fake:
        _, held = _call(fake.url, "POST", "/council/v2/specs", GATE)
        _, built = _call(fake.url, "POST", "/council/v2/contexts", {"model": "toy", "context": ctx})
        status, _ = _call(fake.url, "PUT", "/council/v2/pins/t", {"snapshots": [built["head"]], "ttl_s": 60})
        assert status == 200
        fake.restart()
        assert _call(fake.url, "GET", "/council/v2/specs/" + held["spec_id"])[0] == 200
        assert _call(fake.url, "GET", "/council/v2/pins/t")[1]["snapshots"] == [built["head"]]
        fake.advance_clock(61)
        assert _call(fake.url, "GET", "/council/v2/pins/t")[0] == 404


CTX = {"messages": [{"role": "user", "content": "u", "snap": True}]}


def _read(url, model):
    return _call(url, "POST", "/council/v2/reads", {"model": model, "contexts": [CTX], "suffixes": ["x"], "letters": ["A", "B"]})


def test_the_default_fake_serves_one_model_that_a_request_still_names():
    with gogi.FakeServer() as fake:
        _, ident = _call(fake.url, "GET", "/council/v2/identity")
        assert [(m["model"], m["aliases"], m["status"]) for m in ident["models"]] == [("gogi-toy-1", ["toy"], "ready")]
        status, built = _call(fake.url, "POST", "/council/v2/contexts", {"model": "toy", "context": CTX})
        assert status == 200 and built["model"] == "gogi-toy-1"
        u = built["usage"]
        assert u["kept"] + u["fed"] == u["prompt"] and (u["completion"], u["reasoning"]) == (0, 0)
        status, e = _call(fake.url, "POST", "/council/v2/contexts", {"context": CTX})
        assert (status, e["error"]["param"]) == (400, "model")


def test_each_response_has_a_request_id_and_an_error_body_repeats_it():
    with gogi.FakeServer() as fake:
        a = _call_with_id(fake.url, "GET", "/council/v2/identity")[2]
        b = _call_with_id(fake.url, "GET", "/council/v2/identity")[2]
        assert a and b and a != b
        status, e, rid = _call_with_id(fake.url, "POST", "/council/v2/contexts", {"context": CTX})
        assert status == 400 and rid and e["request_id"] == rid


def test_served_models_route_by_name_or_alias_and_refuse_when_not_ready():
    models = ["day", {"name": "night", "aliases": ["n"], "status": "unloaded"}, {"name": "twin", "seed": 0}]
    with gogi.FakeServer(models=models) as fake:
        _, ident = _call(fake.url, "GET", "/council/v2/identity")
        assert [(m["model"], m["status"]) for m in ident["models"]] == [("day", "ready"), ("night", "unloaded"), ("twin", "ready")]
        status, e = _call(fake.url, "POST", "/council/v2/contexts", {"context": CTX})
        assert (status, e["error"]["param"]) == (400, "model")
        status, e = _read(fake.url, "n")
        assert (status, e["error"]["type"], e["error"]["model"], e["error"]["status"]) == (503, "model_unavailable", "night", "unloaded")
        fake.set_status("night", "ready")
        status, night = _read(fake.url, "n")
        assert status == 200 and night["model"] == "night"
        _, day = _read(fake.url, "day")
        _, twin = _read(fake.url, "twin")
        assert night["reads"][0]["logprobs"] != day["reads"][0]["logprobs"]
        assert twin["reads"][0]["logprobs"] == day["reads"][0]["logprobs"], "the same seed gives the same numbers"
        assert twin["reads"][0]["snapshot"] != day["reads"][0]["snapshot"], "but each served model has its own snapshots"
        fake.set_status("night", "draining")
        assert _read(fake.url, "night")[0] == 503
        fake.set_status("night", "ready")
        status, again = _call(fake.url, "POST", "/council/v2/contexts", {"model": "night", "context": CTX})
        assert status == 200 and again["usage"]["fed"] == 0, "a snapshot outlives a status change"
        fake.restart()
        assert _read(fake.url, "night")[0] == 503, "a restart returns a model to its configured status"


def test_the_fake_refuses_a_bad_model_list_or_status():
    for models, match in [([], "non-empty"), ([{"name": "a", "status": "warming"}], "warming"), (["a", {"name": "b", "aliases": ["a"]}], "a already names"),
                          ([{"name": "a", "colour": "red"}], "unknown members")]:
        with pytest.raises(ValueError, match=match):
            gogi.FakeServer(models=models)
    with gogi.FakeServer() as fake:
        with pytest.raises(ValueError, match="no model nope"):
            fake.set_status("nope", "ready")
        with pytest.raises(ValueError, match="is not one of"):
            fake.set_status("gogi-toy-1", "asleep")


def test_the_fake_reports_and_keeps_the_limits_it_is_given():
    with gogi.FakeServer(context_tokens=512, decision_context_tokens=256, pin_bytes=4096, max_snapshots=2) as fake:
        _, ident = _call(fake.url, "GET", "/council/v2/identity")
        limits = ident["models"][0]["limits"]
        assert (limits["context_tokens"], limits["decision_context_tokens"], ident["limits"]["pin_bytes"]) == (512, 256, 4096)
        status, e = _call(fake.url, "POST", "/council/v2/contexts", {"model": "toy", "context": {"messages": [{"role": "user", "content": "x" * 600}]}})
        assert (status, e["error"]["type"]) == (413, "context_full")
        heads = [_call(fake.url, "POST", "/council/v2/contexts", {"model": "toy", "context": {"messages": [{"role": "user", "content": f"u{i}"}]}})[1]["head"] for i in range(3)]
        assert _call(fake.url, "GET", "/council/v2/snapshots/" + heads[0])[0] == 404, "past max_snapshots the oldest is evicted"
        assert _call(fake.url, "GET", "/council/v2/snapshots/" + heads[2])[0] == 200
        big = {"messages": [{"role": "user", "content": "y" * 200}]}
        _, b = _call(fake.url, "POST", "/council/v2/contexts", {"model": "toy", "context": big})
        status, e = _call(fake.url, "PUT", "/council/v2/pins/t", {"snapshots": [b["head"]], "ttl_s": 60})
        assert (status, e["error"]["param"]) == (413, "pin_bytes")
