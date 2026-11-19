# How this is built

The workspace this crate comes from is written by one person with
[Claude Code](https://claude.com/claude-code) as the implementer. This document describes the
arrangement that makes that work for code where a mistake costs money or moves a car: who
decides what, what the agent is not trusted to do on its word, and what the arrangement has
caught. Numbers are as of 2026-11-20. They were counted from the private repository's commit log
and from the session transcripts, not estimated, but neither is in this repository, so a reader
cannot recount them here. What can be checked from this clone is said to be so.

## Division of labour

| The human | The agent |
|---|---|
| Writes the goal and the acceptance check, as numbers and with an outside judge | Brainstorms the spec, finds prior decisions and external cases |
| Rules on trade-offs; each ruling is recorded | Plans, implements test-first, runs the checks |
| Approves everything irreversible: deploy, push, publish, a command to the car | Reviews with fresh-context reviewer agents and reproduces findings |
| Judges results against the real car, the bill and the map | Replays recorded data through old and new binaries and reports the difference |

The agent writes almost all of the code. Of 1,515 non-merge commits since 2026-07-29, 1,411
(93%) are co-authored by Claude. By type, the largest groups are 603 `feat`, 583 `fix`, 114
`test` and 111 `docs`. There are 186 tagged production deploys.

In the eleven days of transcripts on the current development machine (2026-11-09 to 11-19, 53
sessions in this workspace):

| | |
|---|---|
| Human turns | 720 |
| Tool calls by the agent | 12,499 (17 per human turn; median 7 between two human interventions, 90th percentile 38) |
| Reviewer-agent runs | 54 (35 code reviews, 19 design reviews) |
| Times a hook sent the agent's work back | 184 |
| Tool calls a hook refused outright | 46 |
| Commits / tagged deploys in that window | 161 / 22 |

The interesting rows are the two about hooks. They are the arrangement doing its job without
the human in the loop. (The deploys were each approved by the human.)

## The loop

1. **Spec.** A feature starts as a written spec. It must state the invariant it touches, the
   acceptance check as a number, and an *outside judge*: something other than our own code that
   decides whether it worked (the real car, the charging bill, a satellite image, Tesla's
   documentation). A threshold must come with a positive example and a *nearby negative* one. A
   threshold tuned on positives alone once let the adjacent bad case through.
2. **Design review.** Before any code, a read-only reviewer agent checks the spec against the
   code, citing `file:line`, with a fixed checklist: invariants, gates, deploy order, real-data
   evidence.
3. **Plan.** The plan names its *Review Focus*: the places the author believes are most likely
   wrong.
4. **Implementation.** Test first. The test must be seen to fail (a "negative control") before
   the code that makes it pass is written, and again later with the guard removed.
5. **Machine checks.** One script runs format, clippy with warnings as errors, tests, doctests,
   the invariant gates and the MSRV build, and prints an exit code and pass/fail counts per step.
   "Done" means that script's output, quoted.
6. **Code review.** The depth of review is set by the paths that changed, not by the author:
   `risk_tier.py` sorts a diff into quick, standard or deep. Deep (commands, paid calls, auth,
   privacy, deploy units, the gates themselves) gets two reviewers with split perspectives and a
   security review. Reviewers start with a fresh context and may not edit.
7. **Replay.** A change to a judgment (when an alert fires, what counts as "arrived") is run
   against recorded data with the old and the new binary, and the counts per alert kind are
   compared and checked against a ledger of known-correct outcomes.
8. **Deploy.** Asked every time. Hash before and after, a backup, a tag on the deployed commit,
   one line in the runbook.

## What holds the agent to it

An instruction in a prompt is a request. These are not requests.

**Gates.** 67 checks that the commit hook and the completion check run, written as absence
claims ("no crate but X names Y"). They are described in [safety-boundaries.md](safety-boundaries.md). This repository
runs a small set of its own in CI: [`scripts/check_invariants.py`](../scripts/check_invariants.py),
where every gate has a self-test that plants the violation and expects the gate to fail.

**Hooks.** Run by the harness around the agent's tool calls, so the agent cannot skip them. The
list below is the private workspace's. This repository registers two of them in
`.claude/settings.json`: the refusal of bulk staging and the format-on-edit (without the clippy
run); its comment-language rule is a CI gate instead of a hook.

- Before an edit: refuse a comment that is not in English (the gate that keeps this crate's
  source readable to outside contributors).
- Before a shell command: refuse `git add -A` and its variants, because local capture output can
  contain raw API responses.
- After an edit: format the file, and run clippy on the crate in the background, waking the
  agent if a lint appears.
- When the agent finishes: a second model audits the final report and sends it back if it
  contains a claim without a source.

**Source tags.** Every factual sentence in a report carries one of three tags: *checked directly*
(with the command or `file:line`), *reported by* (a subagent, a reviewer, memory, a handoff
note), or *guess*. A subagent's "tests pass" stays "reported" until the main session reruns it.
This rule exists because of a measurement: in one week, 147 of 182 findings by the stop hook
were claims with no source.

**Reviewers that are themselves tested.** The reviewer agents have fixed checklists and do not
fix what they find; a gate checks that their definitions grant no edit tool. (They keep a shell
to run tests, so this guards against accidents and is not a sandbox.) They are also scored.
`review_eval` holds twelve cases: eleven real defects that reached the main branch, each with the
commit range that introduced it, and one clean change to count false alarms. A change to a reviewer's definition, to the review standard or to
the model is kept only if the score does not drop. The copy that is scored has no memory, so
that findings remembered from the last run cannot inflate the result.

**Lessons become structure.** The principles file admits a rule only when it comes from an
actual correction, with the date. A lesson that had to be said twice is moved out of prose and
into a gate, a hook or an evaluation case, and the move is shown to work by a negative control.

**Approval is not transferable.** An approval relayed by another session or by a bot is not an
approval. Irreversible actions are confirmed by the operator directly, every time.

## Rules of evidence

Most of what goes wrong with an agent is not bad code. It is a confident sentence that nobody
checked. These rules are aimed at that.

- **A zero needs a positive control.** "No matches" is reported together with a value from the
  same query that must be non-zero. A check whose name is wider than its scope turns green and
  lies.
- **A test needs a negative control.** A test that passed the first time it ran has proved
  nothing yet.
- **A cause needs a reproduction.** "It failed because X" is a guess until X has been made to
  fail on purpose. If two fixes do not work, write down the assumption both shared and test
  that.
- **Counts are measured before they are argued from.** "This is rare" is checked against the
  recorded data before a design relies on it. Over 1,600 unit tests once passed while the
  recorded data still exposed two defects.
- **Judged from outside.** The code does not get to say it is right. A threshold is confirmed
  against something it did not come from.

## Defects this caught in this crate

The original specifications, plans and hand-over notes for this crate are in
[docs/design/](design/README.md), in Korean as they were written. The double-charge fix and its
test were made in a later sub-project (A5) whose documents are not published; the A3
hand-over note already names the risk.

Each of these is recorded in the source of this repository, next to the code or test that now
prevents it.

**A double charge, and then its over-correction** (`client.rs`, `client/tests.rs`). The 401 and
412 recovery paths re-sent the request. They predated the billed endpoints, and nobody had
connected them to pricing, so a billed request could be charged twice. The first fix skipped
recovery for billed requests altogether. Review then measured the consequence with two
consecutive calls: a caller that used only billed endpoints never healed, and every call was a
fresh billed failure. The rule was narrowed to "do not re-send; do repair". The same episode
corrected the documentation, which had blamed 408 retries for double billing. A 408 was never
retryable; the path that actually sent twice was the recovery.

**A test that could not fail** (`a_412_on_a_paid_endpoint_is_not_re_fired`). The first fixture
used one mock server that answered 412 once and then 200. The second call succeeded whether or
not recovery had happened, because the mock had simply run out of 412s. The fixture was rebuilt
with two servers so that success on the second call is possible only if the region was really
re-resolved, and the test asserts the number of region lookups so that "no re-send" cannot be
confused with "could not resolve".

**A refresh token one log line from disclosure** (`auth/mod.rs`, `Refreshed`). A final review
found the type that carries both tokens still deriving `Debug`, at the point in the transport
layer where a debug log is most likely to be added. `Debug` is now written by hand and
destructures the struct, so a new field cannot be added without deciding how it prints.

**A guard on a guard that looked the wrong way** (`lib.rs`, `clippy_toml_forbids_every_mutating_method`).
The test that protects the deny-list searched the raw file for each method name. Commenting an
entry out left the name in the file, so the test still passed. It now strips comments first.

**A truer error replaced by a later one, and a test that kept passing for the wrong reason**
(`Client::send`). After a 401 from the server, a failure inside the token layer replaced the
error: the caller was told "refresh suppressed" for what was a refusal by the server, and on a
billed endpoint the replacement would have reported "not billed". The earlier error is now the
one returned. The test written for that fix later stopped reaching the line it guarded, because
billed requests no longer take the retry path, yet it kept passing. A reviewer showed it by
deleting the fix: the test still passed. A second test, on a free endpoint and judged by the
message, now fails without it
(`a_suppressed_recovery_prefers_the_401_over_the_token_layer_error`).

**An error message that sent users the wrong way** (`Error::MissingScope`). The message printed
`(held: [])` because the check returns a verdict and not the parsed claims. It told a user with
the right scopes that their token had none. The field was removed.

## What this does not do

- It does not make the agent's review a proof. Reviewers miss things; that is why missed defects
  become evaluation cases and why the gates exist.
- The gates read source text. They stop mistakes, not intent.
- Replay only covers situations that were recorded.
- It is one person's project. No second human reviews the code, which is what the OpenSSF
  Scorecard's Code-Review check correctly reports for this repository.
- The human is still the bottleneck for judgment, and is supposed to be.

## In this repository

| File | Role |
|---|---|
| [`AGENTS.md`](../AGENTS.md) | What an agent must know and must not break here. |
| [`REVIEW.md`](../REVIEW.md) | The review standard: severity by user effect, required evidence, failure shapes seen in this crate. |
| [`scripts/check_invariants.py`](../scripts/check_invariants.py) | Invariant gates with self-tests; runs in CI. |
| [`scripts/risk_tier.py`](../scripts/risk_tier.py) | Sorts a diff into quick, standard or deep review by the paths it touches. |
| [`.claude/`](../.claude) | Hooks, and the definition of a reviewer agent that is given no edit tool. |
