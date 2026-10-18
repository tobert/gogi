# Where the implementations disagree

This file lists where mkviolet or kaijutsu differ from `docs/council-api.md` and `gogi-proto`
today. Each item is an observation from the code unless it says "inferred". An item leaves this
file when the two agree.

Both decode and validate with `gogi-proto` and pool with its `math`, so most of the contract
cannot disagree. mkviolet runs `math::verify` on every decision before it responds, and passes
`gogi-conformance` with 0 FAIL. kaijutsu pins a gogi rev, so it is behind whenever gogi moves
ahead of that rev.

| # | Where | mkviolet | kaijutsu | gogi-proto |
|---|---|---|---|---|
| 1 | A reply that ends with `finish: context_full` | Its prompt and reply hold `context_tokens` + 1 tokens: its decode stops at the end of its slot, one past identity's `context_tokens` (a live generation: prompt 65,501, completion 35, `context_tokens` 65,535). | n/a | Prompt and reply hold exactly `context_tokens`, the most the contract allows. `gogi-conformance` checks it when a generation it runs ends with `context_full`; its own requests do not reach that limit. |
