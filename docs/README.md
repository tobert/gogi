# gogi docs

**gogi** (合議（ごうぎ）, "deliberation by a panel") defines the council v2 HTTP contract. [nagaya](https://github.com/tobert/nagaya) is the reference server implementation and [kaijutsu](https://github.com/tobert/kaijutsu) is the first client; both depend on `gogi-proto`.

| File | Contents |
|---|---|
| [`council-api.md`](council-api.md) | Normative council v2 HTTP contract: identity, contexts, snapshots, pins, objects, specs, decisions, raw reads, generations, embeddings, control vectors, layer loops, and cycles. |
| [`gogi-fake.md`](gogi-fake.md) | The fake server and the deterministic toy model, chat template, and reply parser in `gogi-toy`. |
| [`disagreements.md`](disagreements.md) | Known divergences between implementations and the contract. |

## Crates

| Crate | Purpose |
|---|---|
| `gogi-proto` | Zero-I/O contract library: wire types, request/spec validation, canonical JSON and `spec_id`, `f64` pooling math, and response verification. |
| `gogi-toy` | Zero-I/O deterministic toy model: tokenizer, chat template, model state, logits, and reply parser. |
| `gogi-fake` | Fake council v2 HTTP server over `gogi-toy` for client and conformance testing (not an engine plugin framework). |
| `gogi-py` | PyO3 bindings over `gogi-proto` and `gogi-fake` for Python clients and tests (built with `maturin`; excluded from default `cargo test`). |
| `gogi-conformance` | Black-box conformance suite (`gogi-conformance <url>`) printing `ok`, `skip`, or `FAIL` for each check. |

See [`council-api.md`](council-api.md) ("Terms") for canonical vocabulary and [`../AGENTS.md`](../AGENTS.md) for contributor guidelines.
