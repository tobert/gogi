# gogi docs

gogi (合議, "deliberation by a panel") holds the council API contract.
kaijutsu is the client and mkviolet is a server; both speak it, and both
depend on `gogi-proto`.

| File | Holds |
|---|---|
| `council-api.md` | The contract: contexts sent whole, snapshots, pins, specs, decisions, raw reads, generations. |
| `gogi-fake.md` | The fake server, and the toy template, model, and reply parser it serves from `gogi-toy`. |
| `disagreements.md` | Where mkviolet or kaijutsu differs from the contract today. |

## Crates

| Crate | Is |
|---|---|
| `gogi-proto` | The contract as code, with no I/O: wire types, message and spec validation, canonical JSON and `spec_id`, the pooling math and the check that a decision's numbers obey it. |
| `gogi-toy` | The toy model as a library, with no I/O: tokens, tokenizer, chat template, model state and logits, and a reply parser. |
| `gogi-fake` | A fake council API server over a toy deterministic model, for client tests and for testing the conformance suite. Not a framework; no server plugs into it. |
| `gogi-py` | Python bindings over `gogi-proto` and `gogi-fake`: validation, `spec_id`, the pooling math, and `gogi.FakeServer` for client tests. Built with maturin; not in the default workspace build. |
| `gogi-conformance` | `gogi-conformance <url>` checks a live server against the contract and prints `ok`, `skip`, or `FAIL` for each check. |

## Terms

`council-api.md`, "Terms", defines the contract's words, and the code uses
them with the same meanings. `AGENTS.md` has the writing rules and, under
"Vocabulary", the words that replace ones that drifted.
