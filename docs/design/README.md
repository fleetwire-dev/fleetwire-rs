# Design history

These are the original specifications, implementation plans and hand-over notes the `fleetwire`
crate was built from, between 2026-07-24 and 2026-08-03. They are published as they were written,
in Korean, the maintainer's working language. They were not rewritten for publication; the only
changes are the redactions listed at the end.

They are here because the source cites them. A comment that says "design §9.2", "REQ-18",
"A3 Task 9" or "O17" points into these files.

## How the crate was built

Each step below was written by a coding agent and reviewed. A step was not started until the
human had approved the one before it.

1. **One specification for the whole client** (`specs/fleetwire-read-client/`), in three files:
   requirements with numbered `REQ-n`, a design with numbered sections, and a task list. It was
   reviewed, corrected, and narrowed to the sixteen GET endpoints that a real account could
   verify.
2. **An evidence gate before any code** (`specs/2026-07-29-...-o1-endpoint-evidence-matrix.md`).
   Each endpoint's scopes, billing category, content type and pagination were settled from
   evidence. This step corrected the specification: only two of the sixteen endpoints turned out
   to be billed. The corrected specification was then regenerated.
3. **Then one sub-project at a time**, A1 to A4. Each has three documents:
   - a **design** for that layer;
   - an **implementation plan**: tasks, each with the tests to write first, the gates that must
     pass before the commit, and the review rounds;
   - a **carry-forward** note written at the end. It records what was decided, what is still open
     (the `O` items), what the final review left as minor, and which testing habits the next
     sub-project inherits.

The carry-forward notes are the most useful documents to read first. They show what each step got
wrong and what changed in the next one.

## Documents

| Date | Document | What it is |
|---|---|---|
| 07-24 | [specs/2026-07-24-fleetwire-read-client-design.md](specs/2026-07-24-fleetwire-read-client-design.md) | The first design, before the specification was split into three files |
| 07-24, corrected 07-30 | [specs/fleetwire-read-client/requirements.md](specs/fleetwire-read-client/requirements.md) | `REQ-1` to `REQ-22` |
| 07-24, corrected 07-31 | [specs/fleetwire-read-client/design.md](specs/fleetwire-read-client/design.md) | The design the code calls "design §n" (for example §9.2, the billing judgment table) |
| 07-24, corrected 07-30 | [specs/fleetwire-read-client/tasks.md](specs/fleetwire-read-client/tasks.md) | The task breakdown |
| 07-28 | [specs/2026-07-28-fleetwire-oss-shell-scaffold-design.md](specs/2026-07-28-fleetwire-oss-shell-scaffold-design.md) | The repository scaffold: licence, CI, release guards |
| 07-29 | [specs/2026-07-29-fleetwire-o1-endpoint-evidence-matrix.md](specs/2026-07-29-fleetwire-o1-endpoint-evidence-matrix.md) | **O1**, the evidence gate for the sixteen endpoints |
| 07-30 | [plans/2026-07-30-fleetwire-a1-core-types.md](plans/2026-07-30-fleetwire-a1-core-types.md) | A1 plan: core types, scopes, pricing, redaction |
| 07-31 | [specs/2026-07-30-fleetwire-a1-carry-forward.md](specs/2026-07-30-fleetwire-a1-carry-forward.md) | A1 hand-over |
| 07-31 | [specs/2026-07-31-fleetwire-a2-token-layer-design.md](specs/2026-07-31-fleetwire-a2-token-layer-design.md) | A2 design: token storage, rotation, single-flight refresh |
| 07-31 | [plans/2026-07-31-fleetwire-a2-token-layer.md](plans/2026-07-31-fleetwire-a2-token-layer.md) | A2 plan |
| 07-31 | [specs/2026-07-31-fleetwire-a2-carry-forward.md](specs/2026-07-31-fleetwire-a2-carry-forward.md) | A2 hand-over |
| 08-02 | [specs/2026-08-02-fleetwire-a3-http-transport-design.md](specs/2026-08-02-fleetwire-a3-http-transport-design.md) | A3 design: transport, region resolution, 401/412 recovery, retries |
| 08-03 | [plans/2026-08-02-fleetwire-a3-http-transport.md](plans/2026-08-02-fleetwire-a3-http-transport.md) | A3 plan |
| 08-03 | [specs/2026-08-03-fleetwire-a3-carry-forward.md](specs/2026-08-03-fleetwire-a3-carry-forward.md) | A3 hand-over |
| 08-03 | [specs/2026-08-03-fleetwire-a4-endpoints-design.md](specs/2026-08-03-fleetwire-a4-endpoints-design.md) | A4 design: the sixteen endpoints, the 408 and pagination measured live |
| 08-03 | [plans/2026-08-03-fleetwire-a4-endpoints.md](plans/2026-08-03-fleetwire-a4-endpoints.md) | A4 plan |
| 08-03 | [specs/2026-08-03-fleetwire-a4-carry-forward.md](specs/2026-08-03-fleetwire-a4-carry-forward.md) | A4 hand-over, with the open items O8 to O20 |

Dates are when the document was first committed and, if later, last corrected.

## Reading a citation in the code

| The code says | Look in |
|---|---|
| `REQ-n` | `specs/fleetwire-read-client/requirements.md` |
| `design §n` | `specs/fleetwire-read-client/design.md`, unless the comment names a sub-project |
| `A2 …`, `A3 …`, `A4 …` with `Task n` or `fix round n` | the plan of that sub-project |
| `I1`, `I2` (invariants of the token layer) | the table of invariants in the 2026-07-24 design, carried into the A2 design |
| `spec §0 C1` and similar in `net.rs` | a later specification for the move to a synchronous client, not published here |
| `O1` | the evidence matrix |
| `O8` to `O20` | the carry-forward notes, and the crate's top-level rustdoc, which keeps the current list |
| `A5 …`, `A5c-2a T23` | the capture tool built on top of the crate. Its documents describe a private component and are not published. |

Some documents link to files that are not here: other private documents, or raw evidence kept
in a working folder at the time. Those links are left as they were.

## Redactions

All replacements were made by one script that checks each replacement count, and the result was
scanned again before publication. The changes:

- the real client id of the Tesla developer app, in four test lines of the A3 plan, is replaced
  with the made-up id the public tests use;
- a company and a personal e-mail address, in one line of the scaffold design, are replaced with
  a description;
- the real vehicle's VIN prefix, in one line of the A4 design, is shortened to its first three
  characters;
- local paths (`~/…`, `/home/…`) are replaced with `<workspace>` or `<docs repo>`, and
  `docs/superpowers/` with `docs/design/`;
- the reasoning behind the author-identity decision (D7) is removed; the decision itself stays;
- the names of the review tools, and the review-history sections that listed their findings
  round by round, are removed. The corrections those reviews led to stay in the text. The
  release research document that the scaffold design links to is not published.

Nothing else was changed. That includes mistakes the later documents correct.
