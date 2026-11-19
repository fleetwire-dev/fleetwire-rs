# REVIEW.md — the review standard

Reviewers, human or agent, read this first. The question is not "did someone read the code". It
is **"what can go wrong with this change, and which check should have caught it?"**

## 1. Depth is set by the change — `scripts/risk_tier.py <base> [head]`

| Tier | What changed | Review |
|---|---|---|
| quick | documentation, tests, agent instructions | the machine checks only |
| standard | any other code | one reviewer with a fresh context (three with split perspectives above 2,000 changed lines) |
| deep | the send loop, pricing, auth, redaction, transport, the sealed trait, dependencies, CI, the gates | two reviewers with split perspectives, plus a security pass |

The author does not lower a tier. A reviewer may raise one.

## 2. What a finding looks like

- **First line is the count:** `Critical N · Important N · Nit N · Pre-existing N`.
- Each finding has a severity, a `file:line`, a **failure scenario** (which input or state leads
  to which wrong result), whether it was reproduced (test name or command), and a suggested fix.
  A finding without a `file:line` is not raised. What was not checked directly is labelled a
  guess.
- **Severity is the effect on the user**, not how the spec was worded.
  - **Critical:** a token, VIN or coordinate can reach a log, an error or a preview; a request
    other than GET can reach the Fleet API; a billed request can be sent twice; the token can be
    sent to a host off the allow-list; a token with a command scope can be used.
  - **Important:** the billing judgment is wrong (spend under- or over-reported); an error
    message sends the user the wrong way; behaviour differs from a measured response; a
    documented guarantee is weaker than stated.
  - **Nit:** naming, wording, structure. **At most five**; the rest is "and N more".
  - **Pre-existing:** a defect this change did not introduce (confirmed with `git blame`). It is
    listed separately and not charged to the author.
- Style and formatting are not raised. rustfmt, clippy and the `error-style` gate own them.
- Add one line each for what could not be judged and for the perspectives that came up clean.

## 3. Failure shapes this crate has actually had — look for these first

| Shape | What happened | What catches it now |
|---|---|---|
| A recovery path that re-sends | 401 and 412 recovery re-fired a billed request: a second charge. | `a_401_on_a_paid_endpoint_is_not_re_fired` and its 412 twin count requests per call. |
| A fix that over-corrects | Skipping recovery entirely for billed requests meant a billed-only caller never healed, and every call was a new billed failure. | The same tests assert that the *next* call succeeds. |
| A fixture that cannot fail | One mock answered 412 once and then 200, so "recovered" and "the mock ran out" looked the same. | Two servers; the test also asserts the number of region lookups. |
| `derive(Debug)` on a secret | The refresh response still derived `Debug` where a debug log was most likely. | Hand-written `Debug` that destructures; the `secret-debug` gate. |
| A guard test that reads the wrong thing | The deny-list test searched raw text, so a commented-out entry still passed. | Comments are stripped before the check. |
| A later error hiding an earlier, truer one | A token-layer failure replaced the server's 401: "refresh suppressed" for an auth refusal, and "not billed" on a billed endpoint. | The earlier response error is the one returned (`a_suppressed_recovery_prefers_the_401_over_the_token_layer_error`). |
| A test passing for a different reason | After billed requests stopped taking the retry path, the test guarding that fix no longer reached the line. Deleting the fix left it green. | A second test that does reach the line; when a rule changes, re-run the negative control of the tests around it. |
| A message that reports an empty set as fact | `(held: [])` was printed by a check that never parsed the claims. | The field was removed. |
| A value "fixed" in the unsafe direction | Raising a too-short token lifetime would send a dead token. | Rotation frequency is limited instead; the value is recorded as given and a test asserts it was not touched. |
| A mechanism named wrongly in the docs | "408 retries double-bill" was repeated in the docs long after it was first written; 408 was never retryable. The path that double-sent was 401/412 recovery. | Documentation claims about behaviour need the test or measurement that shows them. |

## 4. Always

- Check each item of the plan's review focus, one by one.
- **Reproduce** a suspicion with a test where possible. A suspicion that was not reproduced goes
  in a separate list as a hypothesis, and nobody downstream may restate it as fact.
- Ask of every new test: **would it fail if the guard were removed?** If you cannot tell, remove
  the guard and run it.
- Look for the **class** of a defect, not the line. One missed `Debug` means checking every type
  that holds a secret.
- For a rule about money, check the **request count**, not only the returned value.
- A statement about how the live API behaves needs its evidence: a recorded response and a date,
  or the word "unverified".

## 5. Stopping

- Review and fix is **three rounds at most**. If an Important finding survives the third, the
  approach is wrong: write down the assumption every attempted fix shared and test that.
- From the second round, review only the fixing commits.
- A Critical or Important finding is fixed with a test that fails first.

## 6. Reviewing the reviewer

In the workspace this crate is exported from, a reviewer definition, the review standard or the
model is changed only together with a re-run against known defects: for each past defect the
reviewer gets the commit range that introduced it, a clean change is mixed in to count false
alarms, and the change is kept only if the score does not drop. The reviewer that is scored must
not have seen the answers. A defect that review missed becomes a new case.

This repository has no evaluation set of its own yet. Until it does, a change to
`.claude/agents/code-reviewer.md` or to this file is reviewed like code, and section 3 is where a
missed defect is recorded.
