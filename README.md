# 合議 gogi

The **council v2** HTTP contract—as a normative specification, reference validation/math libraries, a deterministic toy server, and a 540+ check black-box conformance suite.

**gogi** (合議（ごうぎ）, "deliberation by a panel") is designed so anyone—or their coding agent—can wrap a custom inference engine in **any language** (Rust, Python, C++, Go, Zig), serve council v2, and verify it end-to-end against `gogi-conformance`.

## Why council v2?

Standard chat-completion APIs treat inference as stateless prompt-in, text-out. Council v2 makes model state, logit-level decisions, and engine identity first-class:

- **Content-addressed model state.** Clients mark KV-cache boundaries (`snap: true`) and pass contexts whole or by `parent` context ID. Snapshots, context records, specs, and media objects are immutable BLAKE3-addressed values—restarts and evictions cost re-prefill, never silent state drift.
- **Multi-context logit decisions.** `POST /council/v2/decisions` evaluates one `case` against a reusable question `spec` across up to 8 independent contexts in parallel, returning per-context and pooled (`linear` / `loglinear`) probabilities, `mass`, `confidence`, `agree`, `spread`, and `leave_one_out` ablation. Servers never apply thresholds—clients fit thresholds against the server's exact identity.
- **Strict identity as a contract.** `GET /council/v2/identity` pins `weight_hash`, `tokenizer_hash`, `template`, and `engine` together. Same identity + same inputs = identical raw numbers.
- **Snapshot-safe steering & recurrence.** Optional capabilities (`control_vectors`, `layer_loops`, `cycles`) fold directly into snapshot identity so steered or looped KV states never contaminate plain reads.

## Implementing a server (in any language)

1. **Read [`docs/council-api.md`](docs/council-api.md)** — implement the **Core** surface first, then opt into optional capabilities in `GET /council/v2/identity`.
2. **Use or port the reference math** — `gogi-proto` (Rust) and `gogi-py` (Python) provide zero-I/O request validation, canonical JSON / BLAKE3 `spec_id`, and exact `f64` pooling (`math::pool`, `math::verify`).
3. **Run `gogi-conformance`** — it treats your server as a black box, never assumes your tokenizer or chat template, and skips capabilities you do not declare:
   ```sh
   cargo run --release -p gogi-conformance -- http://127.0.0.1:8091
   ```

### Core vs. optional capabilities

| Layer | Endpoints / Capabilities | What it covers |
|---|---|---|
| **Core** | `GET /identity`, `POST/GET /contexts`, `GET /snapshots/{id}`, `POST/GET/DELETE /specs`, `POST /decisions` | Served models, immutable context records, KV snapshots, question specs, pooled logit decisions |
| **Generations** | `POST /generations` (`generate`, `thinking`, `continuation`, `replies`, `ride`) | Streaming/non-streaming sampled replies, thinking mode, open-message continuations, multi-reply forks |
| **Raw reads** | `POST /reads` (`reads`) | Direct letter logprobs after custom client suffixes |
| **State leases** | `/pins/{owner}`, `/objects/{id}`, `/keeps/{owner}` (`pins`, `objects`, `tiers`, `classes`) | Restart-surviving snapshot pins, content-addressed media (`image/*`, `audio/*`, `video/*`), priority classes |
| **Embeddings** | `POST /embeddings` (`embed`) | Task-conditioned embeddings with configurable dimensions |
| **Research / kernel** | `POST /control-vectors` (`control_vectors`, `layer_loops`, `cycles`) | Residual steering vectors, repeated layer spans, native recurrent cycles |

## Ecosystem

- **[nagaya](https://github.com/tobert/nagaya)** — Reference Rust exokernel and GPU megakernel server implementing council v2 across AMD (`gfx1151`), Intel (`bmg`), and NVIDIA (`sm121`).
- **[kaijutsu](https://github.com/tobert/kaijutsu)** — Agent environment whose policy gates and chats speak council v2.

## Quickstart & examples

Start the deterministic toy server and run the curl walkthrough or the conformance suite:

```sh
cargo run -p gogi-fake -- --listen 127.0.0.1:8091 &
examples/walkthrough.sh
cargo run -p gogi-conformance -- http://127.0.0.1:8091
```

To also verify that specs, pins, and keeps survive restarts:

```sh
cargo run -p gogi-conformance -- http://127.0.0.1:8091 \
  --allow-restart 'systemctl --user restart my-council-server'
```

Python bindings and tour (requires [maturin](https://www.maturin.rs/)):

```sh
cd crates/gogi-py && maturin develop && pytest tests && cd ../..
python3 examples/council_tour.py
```

Run the workspace test suite:

```sh
cargo test --workspace
```

## Repository layout

| Path | Contents |
|---|---|
| `docs/council-api.md` | Normative council v2 HTTP specification |
| `docs/gogi-fake.md` | Specification of the toy model, template, and fake server |
| `docs/disagreements.md` | Known divergences between implementations and the contract |
| `crates/gogi-proto` | Zero-I/O wire types, validation, canonical JSON, BLAKE3 IDs, and `f64` pooling math |
| `crates/gogi-toy` | Deterministic toy model, tokenizer, chat template, and reply parser (no I/O) |
| `crates/gogi-fake` | Fake council v2 HTTP server over `gogi-toy` for client and conformance testing |
| `crates/gogi-conformance` | Black-box live server conformance suite (`ok` / `skip` / `FAIL`) |
| `crates/gogi-py` | PyO3 bindings exposing validation, math, and `gogi.FakeServer` to Python |
| `examples/` | `walkthrough.sh` (curl) and `council_tour.py` (Python) |
| `AGENTS.md` | Contributor and coding-agent guide (vocabulary, rules, workflow) |

## License

Dual-licensed under either the [MIT license](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.
