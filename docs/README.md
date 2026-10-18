# gogi docs

gogi (合議, "deliberation by a panel") holds the council API contract.
kaijutsu is the client and mkviolet is a server; both speak it.

The council v2 draft is edited here; kaijutsu keeps no copy. kaijutsu and
mkviolet both depend on `gogi-proto`.

| File | Holds |
|---|---|
| `council-api-v2.md` | The council v2 draft: contexts sent whole, snapshots, pins, decisions, raw reads, generations. |
| `gogi-fake.md` | The fake server's toy template and model. |
| `disagreements.md` | Where mkviolet, kaijutsu, and the drafts differ. |
| `council-api.md` | Council v1, which v2 builds on. v2 says where it differs. Its words are v1's: there, a context is a record the server holds. |

## Crates

| Crate | Is |
|---|---|
| `gogi-proto` | The contract as code, with no I/O: wire types, message and spec validation, canonical JSON and `spec_id`, the pooling math and the check that a decision's numbers obey it. |
| `gogi-fake` | A fake council v2 server over a toy deterministic model, for client tests and for testing the conformance suite. Not a framework; no server plugs into it. |
| `gogi-py` | Python bindings over `gogi-proto` and `gogi-fake`: validation, `spec_id`, the pooling math, and `gogi.FakeServer` for client tests. Built with maturin; not in the default workspace build. |
| `gogi-conformance` | `gogi-conformance <url>` checks a live server against the contract and prints `ok`, `skip`, or `FAIL` for each check. |

## Terms

`council-api-v2.md`, "Terms", defines the contract's words, and the code uses
them with the same meanings. `AGENTS.md` has the writing rules and, under
"Vocabulary", the words that replace ones that drifted.
