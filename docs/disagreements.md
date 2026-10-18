# Where the implementations disagree

This file lists where mkviolet or kaijutsu differ from `docs/council-api.md` and `gogi-proto`
today. Each item is an observation from the code unless it says "inferred". An item leaves this
file when the two agree.

Both decode and validate with `gogi-proto` and pool with its `math`, so most of the contract
cannot disagree. mkviolet runs `math::verify` on every decision before it responds, and passes
`gogi-conformance` with 0 FAIL. kaijutsu pins a gogi rev, so it is behind whenever gogi moves
ahead of that rev.

Nothing differs today.
