# gogi docs

gogi (合議, "deliberation by a panel") is where the council API contract will
live. Kaijutsu is the client and mkviolet is the server; both speak it.

The council v2 draft is edited here. Kaijutsu keeps a copy in its `docs/`
that follows this one.

| File | Holds |
|---|---|
| `council-api-v2.md` | The council v2 draft: contexts sent whole, snapshots, pins, decisions, reads, generations. |
| `gogi-fake.md` | The fake server's toy template and model. |
| `disagreements.md` | Where mkviolet, kaijutsu, and the drafts differ. |
| `council-api.md` | Council v1, which v2 builds on. v2 says where it differs. |

## Crates

| Crate | Is |
|---|---|
| `gogi-proto` | The contract as code, with no I/O: wire types, message and spec validation, canonical JSON and `spec_id`, the pooling math and the check that a decision's numbers obey it. |
| `gogi-fake` | A fake council v2 server over a toy deterministic model, for client tests and for testing the conformance suite. Not a framework; no server plugs into it. |
| `gogi-py` | Python bindings over `gogi-proto`: validation, `spec_id`, and the pooling math. Built with maturin; not in the default workspace build. |
| `gogi-conformance` | `gogi-conformance <url>` checks a live server against the contract and prints `ok` or `FAIL` for each check. |

## Terms

The documents define the terms. gogi uses them the same way: context, snapshot,
boundary, head, pin, spec, case, read, identity. See
`~/src/kaijutsu/docs/writing.md` for the writing rules.
