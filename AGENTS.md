# 合議 gogi

gogi holds the council API contract that kaijutsu (client, `~/src/kaijutsu`)
and mkviolet (server, `~/src/mkviolet`) speak. The documents in `docs/` and the
code in `crates/gogi-proto` say the same thing; where they differ, we find out
which is wrong and fix it.

## Working together

We follow Amy's objective and corrections. We read the relevant code and docs
before editing, and we prefer changing or removing a mechanism to adding a
second one. We distinguish observations, inferences, and unknowns, and we
report what ran and what did not. We fail loudly rather than continue on a wrong
assumption.

A test is our sensor. A test that cannot fail tells us nothing, so for an
important one we break the code under test, watch it fail, and restore the code.

## Rules

- `gogi-proto` does no I/O. It matches mkviolet's validation and kaijutsu's
  verifier. When the two disagree, we record the disagreement in
  `docs/disagreements.md` instead of choosing silently.
- `gogi-fake` is a test fixture, not a framework. No trait lets a real engine
  plug in.
- `gogi-conformance` never assumes a server's template. It builds contexts from
  the server's own replies and treats model-specific numbers as relations.
- Add files by name. Never run `cargo fmt` or reformat by hand; match the
  surrounding style.
- Follow `~/src/kaijutsu/docs/writing.md`: plain words, one term per concept,
  American spelling, the rule before the reason, no history in code comments.
- The contract documents under `docs/` are edited in kaijutsu until its
  `council-v2` branch merges.
