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


class FakeServer:
    """A fake council v2 server over a toy deterministic model, for client tests.

    ``with gogi.FakeServer() as fake:`` serves on a free local port at ``fake.url``. With
    ``state_dir``, specs, pins, and the snapshots pins keep survive ``restart()``.
    ``docs/gogi-fake.md`` describes the model.
    """

    def __init__(self, addr="127.0.0.1:0", state_dir=None, context_tokens=None):
        self._server = _gogi._FakeServer(addr, None if state_dir is None else str(state_dir), context_tokens)

    @property
    def url(self):
        return self._server.url()

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
