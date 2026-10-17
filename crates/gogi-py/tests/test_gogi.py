import json
import math
import pathlib

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
    with pytest.raises(ValueError, match=r"messages\[0\]: tool_call_id a answers no call"):
        gogi.check_context(bad)
    with pytest.raises(ValueError, match="queue nope names no tool"):
        gogi.check_context({**ok, "queue": "nope"})


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
    # The fixture names contexts by id; a v2 request sends them whole, so build one that
    # names the same reads by index.
    ids = [r["context"] for r in resp["reads"]]
    renamed = json.loads(json.dumps(resp).replace(ids[0], "0").replace(ids[1], "1"))
    ctx = {"messages": [{"role": "user", "content": "u"}]}
    req = {"spec_id": "sha256:" + "a" * 64, "state": "git push origin main", "contexts": [ctx, ctx], "pool": {"method": "loglinear", "weights": "mass"}}
    gogi.verify_decision(req, renamed)
    renamed["answers"]["verdict"]["probabilities"]["ask"] += 1e-6
    with pytest.raises(ValueError, match=r"answers\.verdict\.probabilities\.ask"):
        gogi.verify_decision(req, renamed)
