# gogi docs

gogi (合議, "deliberation by a panel") is where the council API contract will
live. Kaijutsu is the client and mkviolet is the server; both speak it.

Until kaijutsu's `council-v2` branch merges, kaijutsu's copy of these documents
is the one being edited. The files here are a snapshot of kaijutsu commit
`4cba4313` and change only when gogi's code and a live server show that the
document is wrong.

| File | Holds |
|---|---|
| `council-api-v2.md` | The council v2 draft: contexts sent whole, snapshots, pins, decisions, reads, generations. |
| `council-api.md` | Council v1, which v2 builds on. v2 says where it differs. |

## Crates

| Crate | Is |
|---|---|
| `gogi-proto` | The contract as code, with no I/O: wire types, message and spec validation, canonical JSON and `spec_id`, the pooling math and the check that a decision's numbers obey it. |
| `gogi-fake` | A fake council v2 server over a toy deterministic model, for client tests and for testing the conformance suite. Not a framework; no server plugs into it. |
| `gogi-conformance` | `gogi-conformance <url>` checks a live server against the contract and prints `ok` or `FAIL` for each check. |

## Terms

The documents define the terms. gogi uses them the same way: context, snapshot,
boundary, head, pin, spec, case, read, identity. See
`~/src/kaijutsu/docs/writing.md` for the writing rules.
