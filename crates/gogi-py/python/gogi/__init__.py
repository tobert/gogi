"""The council API contract: validation, spec ids, the pooling math, and a fake server.

Contexts, specs, and requests may be Python values or JSON text. A rule broken raises
``ValueError`` with the message mkviolet would return.
"""

import json as _json

from . import _gogi


def _text(x):
    return x if isinstance(x, str) else _json.dumps(x)


def check_context(context):
    _gogi.check_context(_text(context))


def validate_messages(before, messages):
    _gogi.validate_messages(_text(before), _text(messages))


def canonical_json(value):
    return _gogi.canonical_json(_text(value))


def spec_id(spec):
    return _gogi.spec_id(_text(spec))


def parse_spec_id(spec, describe=False):
    return _gogi.parse_spec_id(_text(spec), describe)


def read(logprobs):
    return _json.loads(_gogi.read_json(list(logprobs)))


def pool(reads, method="linear", weights="uniform", values=None):
    return _json.loads(_gogi.pool_json([list(r) for r in reads], method, weights, values))


def verify_decision(request, response):
    _gogi.verify_decision(_text(request), _text(response))


STATUSES = ("ready", "loading", "draining", "unloaded")


def _served_model(m, i):
    if isinstance(m, str):
        m = {"name": m}
    unknown = set(m) - {"name", "aliases", "seed", "status"}
    if unknown:
        raise ValueError(f"models[{i}]: unknown members {sorted(unknown)}; a served model has name, aliases, seed, and status")
    return {"name": m.get("name"), "aliases": list(m.get("aliases", [])), "seed": m.get("seed", i), "status": m.get("status", "ready")}


class FakeServer:
    """A fake council v2 server over a toy deterministic model, for client tests.

    ``with gogi.FakeServer() as fake:`` serves on a free local port at ``fake.url``. With
    ``state_dir``, specs, pins, and the snapshots pins keep survive ``restart()``.
    ``docs/gogi-fake.md`` describes the model.

    ``models`` lists the served models, each a name or a dict with ``name`` and, optionally,
    ``aliases`` (default none), ``seed`` (default its position; two seeds are two models), and
    ``status`` (default ``"ready"``). Without ``models`` the fake serves ``gogi-toy-1``, alias
    ``toy``, so a request may leave out ``model``. ``set_status`` changes one while it serves.

    ``context_tokens`` (default 4096), ``decision_context_tokens`` (default 3072), and
    ``pin_bytes`` (default 1 MiB) are the limits identity reports. Past ``max_snapshots``
    (default 10,000) held snapshots, the oldest unpinned ones are evicted first.
    """

    def __init__(self, addr="127.0.0.1:0", state_dir=None, context_tokens=None, decision_context_tokens=None,
                 pin_bytes=None, max_snapshots=None, models=None):
        models_json = None if models is None else _json.dumps([_served_model(m, i) for i, m in enumerate(models)])
        self._server = _gogi._FakeServer(addr, None if state_dir is None else str(state_dir), context_tokens,
                                         decision_context_tokens, pin_bytes, max_snapshots, models_json)

    @property
    def url(self):
        return self._server.url()

    def set_status(self, model, status):
        """Sets a served model's status, one of ``STATUSES``, as a server does when it loads or
        unloads a model. A request to a model that is not ``ready`` is a ``503``. Snapshots and
        pins stay held. ``restart()`` returns each model to its status in ``models``."""
        self._server.set_status(model, status)

    def advance_clock(self, secs):
        self._server.advance_clock(secs)

    def restart(self):
        self._server.restart()

    def close(self):
        self._server.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
