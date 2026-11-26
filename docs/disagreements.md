# Where implementations disagree

This file tracks known divergences between live implementations ([nagaya](https://github.com/tobert/nagaya), [kaijutsu](https://github.com/tobert/kaijutsu)) and `docs/council-api.md` / `gogi-proto`. Remove an entry once the implementation catches up.

Both `nagaya` and `kaijutsu` decode and validate with `gogi-proto` and pool with its `math` module, so most wire rules cannot diverge. `nagaya` runs `math::verify` on every decision response and passes `gogi-conformance` with 0 failures. `kaijutsu` pins a `gogi` revision, so it lags briefly whenever `gogi` advances ahead of its pin.

- **kaijutsu's spec IDs are SHA-256.** kaijutsu computes `sha256:` over a spec's canonical JSON (`DOC_SPEC_ID`), whereas the contract requires `blake3:` and 64 lowercase hex digits. Inferred: kaijutsu's pinned `gogi-proto` also accepts any `<scheme>:<hex>` snapshot ID.
