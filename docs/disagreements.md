# Where the implementations disagree

This file lists where mkviolet or kaijutsu differ from `docs/council-api.md` and `gogi-proto`
today. Each item is an observation from the code unless it says "inferred". An item leaves this
file when the two agree.

Both decode and validate with `gogi-proto` and pool with its `math`, so most of the contract
cannot disagree. mkviolet runs `math::verify` on every decision before it responds, and passes
`gogi-conformance` with 0 FAIL. kaijutsu pins a gogi rev, so it is behind whenever gogi moves
ahead of that rev.

- **kaijutsu's spec ids are SHA-256.** kaijutsu computes `sha256:` and the hex SHA-256 of a
  spec's canonical JSON (`DOC_SPEC_ID`). The contract's spec id is the content id, `blake3:`
  and the hex BLAKE3 of the same text. Inferred: kaijutsu's pinned `gogi-proto` also accepts
  any snapshot id of the form `<scheme>:<hex>`, while the contract allows only `blake3:` and
  64 digits.
