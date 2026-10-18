# 合議 gogi

gogi holds the council API contract that kaijutsu (the client) and mkviolet
(the server) speak. The documents in `docs/` and the code in
`crates/gogi-proto` say the same thing; where they differ, we find out which is
wrong and fix it.

## Rules

- `gogi-proto` does no I/O. It matches mkviolet's validation and kaijutsu's
  verifier. When the two disagree, we record the disagreement in
  `docs/disagreements.md` instead of choosing silently.
- `gogi-fake` is a test fixture, not a framework. No trait lets a real engine
  plug in.
- `gogi-conformance` never assumes a server's template. It extends contexts only
  with the server's own responses and treats model-specific numbers as relations.
- Add files by name. Never run `cargo fmt` or reformat by hand; match the
  surrounding style.
- A path into another repo names the repo: mkviolet's `src/council_v2.rs`, not
  `src/council_v2.rs`. Never write a path on one machine, such as `~/src/...`.

## Vocabulary

The Terms table in `docs/council-api.md` defines the contract's words.
Documents, code comments, identifiers, error messages, and examples use those
words with those meanings. When a new concept needs a name, add it to that
table first.

These words drifted before. Use the term on the right.

| Do not write | Write |
|---|---|
| held context, stored context | context (the server holds snapshots, never contexts) |
| seat, agent seat, council context | queue context, or context |
| state, for the thing judged | case |
| state, for what a snapshot holds | model state |
| rows, for what a snapshot stores | KV cache, or model state (`math::Row` is a row of the matrix a pool combines) |
| spec layer | the spec as rendered |
| reply, for an HTTP body | response (a reply is what a generation writes) |
| user turn | user message (an agent turn is a run of messages) |
| System 1, System 2 | raw read, generation |
| answer, as a verb for a server or a tool | return, respond with; a tool message gives a call's result |

"Council context" is kaijutsu's term for the context that holds a character's
guidance. gogi does not use it.

Some error messages in `gogi-proto` are mkviolet's text, such as the queue
errors in `decision.rs`. Change those only together with mkviolet, or record
the difference in `docs/disagreements.md`.

## Writing

These rules apply to documents, code comments, error messages, and help text.

- Use plain words, not figures of speech. The meaning must come from the words
  alone, for a reader with partial context or English as a second language.
- Use one word for each concept and keep it. Do not vary a word for style.
- Use American spelling: `modeled`, not `modelled`.
- Give specific values: the status code, limit, default, and condition. Write
  "a context with no messages is a `400`", not "an empty context fails".
- Put the rule before the reason, so a reader who stops early keeps the rule.
- Show the correct example first. When a wrong example is needed, put it after
  the correct one and mark it.
- An error leads with the consequence, names the condition, and suggests the
  next step when it is known.
- Code comments state the rule the code follows. They hold no dates, rulings,
  or commit hashes; those belong in `git log`.
