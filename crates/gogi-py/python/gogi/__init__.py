"""The council API contract: validation, spec ids, and the pooling math.

Contexts, specs, and requests may be Python values or JSON text. A rule broken raises
``ValueError`` with the message mkviolet would answer with.
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
