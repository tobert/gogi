# AGENTS.md

Contributor and coding-agent guide for **gogi** (合議（ごうぎ）)—the language-agnostic **council v2** HTTP contract, reference libraries (`gogi-proto`, `gogi-py`), toy server (`gogi-fake`), and black-box conformance suite (`gogi-conformance`).

Start with [`README.md`](README.md) for the overview and [`docs/council-api.md`](docs/council-api.md) for the normative contract.

## Goal & design principles

gogi exists so any team or coding agent can wrap a custom inference engine in **any language** and verify compliance with `gogi-conformance`.

- **Docs and code are one contract.** `docs/council-api.md` and `crates/gogi-proto` must agree. When they differ, determine which is wrong and fix it—or record known client/server lag in `docs/disagreements.md`.
- **`gogi-proto` does no I/O.** Pure wire types, validation, canonical JSON, BLAKE3 content IDs, and `f64` pooling/verification math.
- **`gogi-fake` is a test fixture, not a framework.** Do not add engine plugin traits to it.
- **`gogi-conformance` is strictly black-box.** Never assume a server's tokenizer or chat template; extend contexts only from the server's own responses and check model-specific numbers via mathematical relations.
- **Public repository hygiene.** Never commit private hostnames, real IPs (use RFC 5737 blocks `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), home paths (`~/src/...`), or old codenames (`mkviolet`, `lfm2d`, `/mk/v1`, `mkv-letters-1`). When citing another repository, name it explicitly (e.g., `nagaya`'s `crates/nagaya-council/`).

## Canonical vocabulary

The **Terms** table in [`docs/council-api.md`](docs/council-api.md) is the source of truth. Use the canonical term consistently across docs, code comments, identifiers, error messages, and examples. When introducing a new concept, add it to that table first.

| Avoid | Use |
|---|---|
| held context, stored context | **context record** (the server's stored record of a context, named by its context ID) |
| seat, agent seat, council context | **queue context**, or **context** (`council context` is kaijutsu-specific) |
| state (for the thing judged) | **case** |
| state (for what a snapshot holds) | **model state** |
| rows (for what a snapshot stores) | **KV cache** or **model state** (`math::Row` is a matrix row in pooling) |
| spec layer | **the spec as rendered** |
| reply (for an HTTP body) | **response** (`reply` is what a generation writes) |
| user turn | **user message** (`agent turn` is a run of assistant/tool messages) |
| System 1, System 2 | **raw read**, **generation** |
| answer (as a verb for a server/tool) | **return** / **respond with** (a tool message gives a call's result) |

## Style

- **Clarity and high information density** in docs, comments, error messages, and commit messages. Avoid filler.
- **Be concrete:** state exact status codes, limits, defaults, and conditions ("a context with no messages is a `400` naming `messages`", not "an empty context fails").
- **Rule before reason; valid example before invalid example.**
- **Error messages** lead with what failed, name the violated rule/param, and state how to fix it. Keep wire error messages aligned with `nagaya` and `kaijutsu`, or document intentional transitions in `docs/disagreements.md`.
- **Code comments** state the invariant or rule—no dates, session notes, or commit hashes.
- Use American English (`modeled`, not `modelled`).

## Workflow & checks

- Edit on a feature branch, run checks, merge to `main`, and (when updating `nagaya`) bump the pinned `rev` in `nagaya/Cargo.toml`.
- Stage files explicitly by name (`git add <file>`). Never use `git add -A` or `git add .`.
- Run before committing:
  ```sh
  cargo test --workspace
  ```
  If you touch `crates/gogi-py` (and `maturin`/`pytest` are installed):
  ```sh
  cd crates/gogi-py && maturin develop && pytest tests
  ```
- End commits with `Co-Authored-By: <Model> <email>` when authored by an agent.
