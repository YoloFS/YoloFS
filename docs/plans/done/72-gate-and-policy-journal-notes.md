# 72 — Gate Results and Policy Configuration Journal Notes

## Goal

Replace the journal's mechanism-specific `A` (ask resolved) and `B` (static
block) notes with two semantic records:

- `G` records the result of an access that reached an interactive or denying
  gate.
- `C` records a live explicit-policy configuration during a mounted session.

The new format should make the journal and paper distinguish access results
from policy changes. It should remain minimal, append-only, and observational
with respect to staged filesystem state.

No backward compatibility is required. The parser will stop accepting `A` and
`B` records.

## Wire Format

Records remain NUL-separated and newline-terminated:

```text
G\0<path>\0<op>\0<result>\n
C\0<path>\0<policy>\n
```

### `G`: gate result

`<op>` keeps the existing encoding:

- `r`: read or execute access
- `w`: write or metadata mutation

`<result>` records both the result and whether an ask occurred:

- `d`: denied directly by a static policy (`deny`, or `read-only` for a
  write); no ask occurred
- `y`: an ask occurred and resolved to allow
- `n`: an ask occurred and resolved to deny, including timeout denial

Emit exactly one `G` for each prompted access and each access denied by a
static policy. Do not emit `G` for accesses passed directly by `allow`,
`read-only`, or the read side of `write-ask`. Keep `hide` unlogged because
logging its path would disclose a hidden name.

`G` intentionally omits the source rule path. It is a self-contained statement
of the gate result, not a reconstruction record for the rule tree.

### `C`: configure policy

`C` stands for **Configure**. `<policy>` is one of the canonical configuration
tokens:

```text
ask | allow | write-ask | read-only | deny | hide | unset
```

`unset` removes the explicit rule at `<path>` and restores inheritance. A `C`
record is an assignment, so the previous policy is deliberately omitted. Do
not emit a record when the requested explicit policy already equals the
current explicit policy.

Emit `C` only when a policy assignment is successfully applied to a live
mount. Applying the configuration while mounting or remounting initializes
the rule tree and must not be reported as a user policy change. An edit made
while unmounted is persisted in `yolofs.toml`, but there is no live session
journal in which to record it.

## Semantics and Invariants

1. `G` and `C` are notes. Neither changes the override tree, participates in
   commit, sets the staging dirty bit, nor causes an automatic snapshot.
2. `G` replaces both old notes without overlap:
   - old `A(..., y)` becomes `G(..., y)`;
   - old `A(..., n)` becomes `G(..., n)`;
   - old `B(...)` becomes `G(..., d)`.
3. `C` describes the live rule-tree transition, not the durable filesystem
   configuration transaction and not mount-time replay. `yolofs.toml` remains
   the source of truth across sessions.
4. Travel changes the staged filesystem branch but does not restore policy.
   Therefore `C` is chronological and non-branching: a policy configuration
   remains effective after travel and must not be dimmed merely because the
   filesystem segment containing it became unreachable.
5. `G` keeps the current access-note range and reachability behavior. It is
   shown in the segment where the access occurred and is dimmed when that
   segment is unreachable.
6. Commit and abort may clear the journal while leaving `yolofs.toml` intact.
   `C` is a session audit event, not a durable history of all policy edits.
7. A failed `G` append remains best-effort audit behavior, matching current
   `A`/`B` handling. A failed `C` append must fail the live rule assignment so
   the journal never claims an incomplete policy transition and the live rule
   tree never contains an unjournaled in-session change.

## Design Changes

### 1. Documentation first

Before implementation, update:

- `docs/staging.md`: replace the A/B format and semantics with G/C, including
  result and policy token tables, dirty-bit behavior, travel behavior, and the
  session-only scope of C.
- `docs/permissions.md`: describe one gate-result record for ask/static paths;
  document live policy configuration records and mount-time suppression.
- `docs/architecture.md`: use `G` and `C` in the journal overview where record
  classes are summarized.
- `../paper/sections/53-permission.tex`: describe the access trace as G records
  and policy evolution as C records.
- `../paper/figures/model.tex`: replace the A/B rows with G/C and use `result`
  as the G field name.
- Any other paper or CLI text found by searching for `A/B`, `Ask resolved`,
  `Blocked`, `Note::Ask`, and `Note::Block`.

The paper should say "every prompted or denied access," not "every gated
access," because direct allows are intentionally not journaled.

### 2. Userspace journal model

In `user/journal/types.rs`:

- Replace `Note::Ask` and `Note::Block` with:

  ```rust
  Note::Gate { path: String, op: Op, result: GateResult }
  Note::Configure { path: String, policy: Policy }
  ```

- Add `GateResult::{DirectDeny, AskAllow, AskDeny}` with strict `d`/`y`/`n`
  wire conversion.
- Reuse the existing permission type for configured policies if it can express
  `unset` cleanly. Otherwise add a journal-specific policy enum rather than
  making the runtime `Perm` type nullable.
- Keep both variants observational to `DirTree`.

In `user/journal/parse.rs`:

- Parse only well-formed G and C records with exactly valid operation, result,
  and policy tokens.
- Remove A/B parsing and tests.
- Continue skipping malformed records without partially constructing notes.

### 3. Kernel gate-result writer

In `kmod/journal.c`, `kmod/yolofs.h`, and `kmod/perm.c`:

- Replace `yolo_journal_ask` and `yolo_journal_block` with one typed
  `yolo_journal_gate` writer.
- Ask allow writes `y`; ask denial and timeout denial write `n`; static denial
  writes `d`.
- Remove rule-path resolution from the static-denial journal path. The rule
  engine may still resolve the source rule for enforcement and prompting, but
  G does not serialize it.
- Preserve exactly-one-record behavior for ask denial.
- Keep static allows and hidden accesses unlogged.
- Exclude G from the staging dirty bit.

### 4. Live policy writer and ioctl intent

Extend the rule-set ioctl input so the caller states whether this assignment is
a user policy configuration that should emit C. The mount/remount rule loader
passes false; `yolo rule ...` passes true. Do not infer intent from mount state
inside the kernel.

For a journaled rule assignment:

1. Resolve and validate the target and policy.
2. Read the current explicit policy under the existing dentry synchronization.
3. Return success without C or a generation bump when old and new are equal.
4. Serialize the C append and live rule publication so concurrent rule ioctls
   have one unambiguous order.
5. Append C before publishing the new dentry policy. If the append fails,
   return the error with the old live policy unchanged.
6. Publish the policy, update rule pinning, and bump `perm.gen`.

The serialization mechanism must be sleepable because journal I/O cannot run
under the current dentry spinlock. Prefer one rule-update mutex in the
permission state over widening the staging journal semaphore's meaning.

The userspace command should report a live rule as applied only after the ioctl
and C append succeed. Configuration-file persistence remains the existing
source-of-truth operation; if its ordering relative to the live ioctl is
changed, document and test rollback behavior rather than allowing silent
config/live divergence.

### 5. Journal indexing and reachability

Keep C at its physical position in journal order, but do not let filesystem
branch reachability mark it dead:

- `Journal::new` should retain C alongside its chronological segment position.
- Tree building, commit planning, staged-change detection, and dirty detection
  ignore C.
- `yolo journal all` renders C normally even when adjacent S/D/R/G records are
  unreachable.
- Range selection may include C by its physical segment interval, but liveness
  filtering must not remove it.
- G continues to follow its segment's reachability.

Do not replay C into the kernel rule tree during travel or mount. The config
loader and live rule ioctl remain authoritative; the journal record is an
audit event.

### 6. CLI presentation

Update `user/changeset.rs`, `user/cmd/journal.rs`, and
`user/cmd/review.rs`:

- Render G as one of `denied`, `asked -> yes`, or `asked -> no`.
- Render C as `configured <path> = <policy>`.
- Match path filters against both record types.
- Deduplicate repeated G notes in review using path, op, and result.
- Do not deduplicate C records; ordered policy assignments are distinct audit
  events. No-op assignments are already suppressed by the kernel.
- Keep both kinds under the existing non-staged/audit presentation and exclude
  them from staged-change counts.

## Tests

### Userspace unit tests

- Round-trip all G result codes and all C policy tokens.
- Reject invalid op/result/policy values and missing fields.
- Verify A and B are no longer parsed.
- Verify G and C do not affect `DirTree`, staged-change detection, commit plans,
  or the dirty summary.
- Verify G deduplication and non-deduplication of ordered C assignments.
- Verify journal and review formatting and path filtering.
- Verify C remains visible when its neighboring filesystem segment is
  unreachable, while G follows segment reachability.

### Kernel and integration tests

- Ask allow emits one `G ... y`.
- Ask deny emits one `G ... n` and no second record.
- Ask timeout emits one `G ... n`.
- Static deny and read-only write denial emit one `G ... d`.
- Directly allowed accesses emit no G.
- Hidden accesses emit no G.
- Every metadata-mutation gate preserves the target-path and `w` semantics.
- `yolo rule` add, change, and unset emit C with canonical policy tokens.
- Reassigning the same explicit policy emits no C and does not bump the policy
  generation.
- Mount and remount configuration application emits no C.
- An unmounted config edit emits no C and is applied normally on the next
  mount.
- Inject or simulate journal-append failure and verify a journaled live policy
  assignment is not published.
- Travel does not revert the live policy and does not mark C unreachable.
- G/C-only activity does not trigger `snapshot --if-changed`.
- Commit and abort behavior remains unchanged.

Update the existing internal and CLI journal-note suites rather than retaining
A/B compatibility assertions. Rename test files and helpers whose names are
specific to A/B when that improves clarity.

## Validation and Review

1. Run focused userspace unit tests while changing the parser, journal model,
   changeset, and renderers.
2. Run focused permission and internal journal tests in the VM.
3. Run `make test-vm` from `filesystem/`.
4. Run `make -C ../paper check` after updating the paper and figure.
5. Perform the mandatory parallel code review for correctness, code quality,
   documentation consistency, missing tests, and adherence to this plan.
6. Address all findings, rerun affected tests, and move this plan to
   `docs/plans/done/` only when implementation, documentation, tests, and review
   are complete.

## Non-goals

- Logging every stat, lookup, readdir, or statically allowed file access.
- Logging hidden paths.
- Reconstructing the rule tree from the journal.
- Restoring policy during travel.
- Keeping policy history across commit/abort session boundaries.
- Supporting old A/B journal files.
