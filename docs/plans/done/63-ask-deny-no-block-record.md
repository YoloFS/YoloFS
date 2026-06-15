# 63 — Disjoint A/B notes + record the blocking rule's path in B

Two related changes to the journal's observational notes:

- **(1) Disjoint A/B** — an `ask`/`write-ask` access that resolves to deny
  must produce exactly one record (the **A** with decision=`n`), never also a
  **B**.
- **(2) Rule path in B** — extend the **B** record to carry the *path of the
  rule* that blocked the access (e.g. the `/etc` in "blocked because of the
  rule on `/etc`"), not just the access path.

## Why

The directory journal has two observational `Note` records:

- `A\0<path>\0<op>\0<decision>\n` — an `ask`/`write-ask`-write was resolved;
  records the y/n decision. Written by `yolo_ask_userspace` (`kmod/perm.c`)
  via `yolo_journal_ask`.
- `B\0<path>\0<op>\n` — an access was blocked by a rule (`-EACCES`). Written
  by callers via `yolo_journal_block`.

**Bug (1):** a denied `ask`/`write-ask` access currently produces *both*
records. `yolo_perm_check_dentry` runs the ask protocol —
`yolo_ask_userspace` writes the **A** (decision=`n`) — then returns `-EACCES`
for the deny decision. The caller sees `-EACCES` and *also* writes a **B**:
- `yolo_open` (`kmod/file.c`)
- `yolo_check_mutate_perm` (`kmod/inode.c`)
- `yolo_setattr` (`kmod/inode.c`)

So a denied ask is double-counted: one A (`n`) + one B. A static `deny` /
`read-only`-on-write block correctly yields only B; an allowed ask yields only
A. Only the ask-deny case is wrong.

**Enhancement (2):** B records only the access target today. When a rule on an
ancestor blocks a deeper path, the user can't see *which* rule fired from the
journal alone — only that "something denied `/etc/x`". Recording the rule path
makes the cause legible ("`/etc/x` blocked by the rule on `/etc`"), aids rule
refinement, and mirrors the `rule_path` the ask protocol already sends the
daemon. We record the rule's **path**, not its mode (`deny`/`read-only`).

## Goals

Make A and B disjoint, per access, and have B name the blocking rule:

- **A** = an `ask` was resolved (y/n). The **sole** record for ask denials.
- **B** = an access blocked by a **static** rule with **no** prompt
  (`deny`, `read-only`-on-write), carrying the rule's path.
- An `ask`/`write-ask` access that resolves to deny produces **exactly one**
  record: the A with decision=`n`. No B.
- New B wire format: `B\0<path>\0<op>\0<rule_path>\n` (4 fields). `<path>` is
  the access target (unchanged); `<rule_path>` is the overlay path of the
  dentry whose rule produced the block.

Invariants after this change:

| Effective perm + op            | Outcome | Records          |
|--------------------------------|---------|------------------|
| `allow`                        | pass    | none             |
| `ask` / `write-ask` → allow    | pass    | A (y)            |
| `ask` / `write-ask` → deny     | -EACCES | A (n)            |
| `deny`                         | -EACCES | B (+ rule_path)  |
| `read-only` + write            | -EACCES | B (+ rule_path)  |
| `hide`                         | -ENOENT | none             |

`<rule_path>` is normally non-empty: a static block always comes from an
explicit rule on some ancestor (`deny`/`read-only`), so `yolo_perm_walk`
always finds a source dentry. Empty only in the defensive/unresolvable case
(treated as best-effort — still emit the B with an empty rule field).

## Non-goals

- **Do not rename the `A`/`B` tags.** They match the on-disk bytes and the
  underlined-letter mnemonic (**A**sk / **B**lock). Descriptions/docs change
  only *together with* the disjoint fix — not before, since today B still
  catches ask-denials and the new wording would mis-state current behavior.
- **Record the rule's path, not its mode.** No `deny`/`read-only` mode field
  on B. (If a mode were ever added: `hide` returns `-ENOENT` at lookup and
  never reaches the block path; `ask` is recorded as A — so neither could
  appear. Out of scope here regardless.)
- No change to the **A** record (it keeps `path`/`op`/`decision`; it does not
  gain a rule_path). The built-in default `ask` has no rule path anyway.
- No change to `yolo_permission`'s decision logic: it returns `0` for
  `ASK`/`WRITE_ASK` and only reaches `-EACCES` for static `deny`/`read-only`
  -write — so it never double-logs an ask. It *does* get the new rule_path
  plumbing (it is a B-emit site).
- No in-kernel dedup / rate limiting changes.

## Design

### (1) Disjoint A/B

The `-EACCES` from `yolo_perm_check_dentry` has two origins the caller can't
tell apart:

1. **Static block** — `yolo_perm_check(perm, …)` returned `-EACCES`. No
   prompt, nothing journaled → caller must write **B**.
2. **Ask-resolved deny** — the ask protocol ran, `yolo_ask_userspace` already
   wrote **A**, then the deny decision was mapped to `-EACCES`. Caller must
   **not** write B.

Add a nullable `bool *ask_resolved` out-param to `yolo_perm_check_dentry`. Set
`*ask_resolved = true` only on the path where `yolo_ask_userspace` returned
`0` (A written). Every static-block return leaves it `false`. Callers gate B
on `err == -EACCES && !ask_resolved`. The return value stays plain `-EACCES`,
so the VFS contract is unchanged.

Rejected: a distinct return code (callers would have to remap to `-EACCES` for
the VFS); re-reading the cached perm post-hoc (racy, and wrong for the
parent-vs-child mutate case).

### (2) Rule path in B

The rule that caused a static block is the nearest ruled ancestor of the
**checked** dentry — the file itself for opens/setattr/`yolo_permission`, but
the **parent** for mutates (create/unlink/…). It is *not* derivable from the
access-target dentry: e.g. parent `/foo`=deny, child `/foo/bar`=allow → the
mutate is blocked by `/foo`'s rule, but walking from the child would wrongly
report `/foo/bar`. So the rule must be resolved from the checked dentry.

Have `yolo_journal_block` take both dentries and resolve the rule path
itself:

```c
int yolo_journal_block(struct yolo_sb_info *sbi, struct dentry *target,
                       struct dentry *checked, enum yolo_op op);
```

- `target` → the `<path>` field (file for opens; child for mutates).
- `checked` → the dentry whose rule gated the access; `journal_block` calls
  `yolo_perm_walk(checked, &rule)`, formats `dentry_path_raw(rule, …)` into
  the `<rule_path>` field, then `dput(rule)`. NULL rule → empty rule_path.

This centralizes rule-path resolution in one place; all four emit sites become
one-liners passing the two dentries. (`journal.c` already calls cross-cutting
helpers like `yolo_preimage_target`, so calling `yolo_perm_walk` fits.) Two
256-byte (`YOLO_PATH_MAX`) stack buffers in `journal_block` (target + rule) —
cheap. The walk is a second O(depth) walk on the rare block path; acceptable.

## Plan

### Docs first (workflow gate)

1. `docs/permissions.md` — in the journal paragraph (around the
   `B\0<path>\0<op>\n` description): B is written **only** for static-rule
   blocks (`deny`, `read-only`-on-write, no prompt) and now carries the
   blocking rule's path; an `ask`/`write-ask` resolved to deny is recorded
   **solely as A (decision=`n`)**, never B. State A/B are disjoint. Use the
   phrasings: A = *"Ask r/w on path, decided y/n"* (covers allowed **and**
   denied asks); B = *"Block r/w on path by the rule at rule_path"*.

2. `docs/staging.md` §"Journal Format" — update the code block, the B table
   row, and the observational-notes paragraph: new B format
   `B\0<path>\0<op>\0<rule_path>\n`; A is the sole record for ask denials; B
   is for static blocks only; `<rule_path>` is an overlay path (same
   namespace as `<path>`). Keep the existing note that `hide` → `-ENOENT`
   is never logged.

3. This plan; move to `docs/plans/done/` once implemented.

### Kernel

1. `kmod/yolofs.h`:
   ```c
   int yolo_perm_check_dentry(struct yolo_sb_info *sbi, struct dentry *dentry,
                              int f_flags, bool *ask_resolved);
   int yolo_journal_block(struct yolo_sb_info *sbi, struct dentry *target,
                          struct dentry *checked, enum yolo_op op);
   ```

2. `kmod/perm.c` (`yolo_perm_check_dentry`): accept `bool *ask_resolved`; set
   `if (ask_resolved) *ask_resolved = true;` right after `yolo_ask_userspace`
   returns `0`, before mapping the decision to `0`/`-EACCES`. Update the doc
   comment.

3. `kmod/journal.c` (`yolo_journal_block`): add `checked` param; resolve the
   rule via `yolo_perm_walk(checked, &rule)`, format its path into the new
   `<rule_path>` field (empty if NULL), `dput(rule)`; write the 4-field B.
   Update the `yolo_journal_block` doc comment and the file-header A/B note
   block (B = static-rule block, now with rule_path; A = sole ask-deny
   record).

4. `kmod/file.c` (`yolo_open`):
   ```c
   bool ask_resolved = false;
   err = yolo_perm_check_dentry(sbi, dentry, file->f_flags, &ask_resolved);
   if (err) {
       if (err == -EACCES && !ask_resolved)
           yolo_journal_block(sbi, dentry, dentry,
                              yolo_open_op(file->f_flags));
       return err;
   }
   ```

5. `kmod/inode.c`:
   - `yolo_check_mutate_perm`: `ask_resolved` gate; on static block
     `yolo_journal_block(sbi, dentry, dentry->d_parent, YOLO_OP_WRITE)`
     (target = child, checked = parent).
   - `yolo_setattr`: `ask_resolved` gate; `yolo_journal_block(sbi, dentry,
     dentry, YOLO_OP_WRITE)`.
   - `yolo_permission`: no gate (never asks); on its `-EACCES` branch call
     `yolo_journal_block(sbi, alias, alias, op)`.

### Userspace (compiler-guided: adding a field to `Note::Block`)

1. `user/journal/types.rs` — `Note::Block { path, op, rule_path: String }`;
   update the doc-comment wire format to the 4-field B.
2. `user/journal/parse.rs` — B branch `fields.len() >= 4`, parse `fields[3]`
   as `rule_path`; update the file-header comment.
3. `user/changeset.rs` — include `rule_path` in `note_key`'s Block key so
   blocks differing only by rule are not deduped together.
4. `user/cmd/journal.rs` — render the rule path in the Block line.
5. `user/cmd/review.rs` (`print_notes`) — include the rule path in the Block
   parenthetical (e.g. `blocked <op> by <rule_path>`).
6. Match sites using `Note::Block { path, .. }` (most of them) compile
   unchanged; only full destructures / field-count code needs the new field.

### Tests

**Failing test first** (gate #3) — in `tests/internals/test_journal_notes.rs`
(it inspects `.yolofs/journal` and has the `journal`/`notes` helpers):

- `ask_deny_records_only_ask_note_no_block` (the bug repro): no-rules session
  (everything `ask`), no daemon (ask → default deny), read `hello.txt`.
  Assert exactly one A note (`…/hello.txt`, read, Deny) **and zero** B notes
  for that path. Fails today (a B is also emitted), passes after the fix.
- **Rewrite** `ask_resolved_to_default_deny_emits_block` → assert the new
  disjoint behavior (no B; one A). Decision confirmed with the user; update
  its name/doc-comment since it no longer exercises a block-emit site.
- B rule-path coverage: with `"/etc"`=deny (or `"/"`=deny), a denied access
  produces a B whose `rule_path` is the rule's path (ends with `/etc`, or is
  `/`). Add as a white-box assertion on the parsed `Note::Block.rule_path`.

**Format-change updates** (B is now 4 fields — keep tests in sync):

- `tests/internals/test_journal_notes.rs::block_record_format`: expect 4
  fields; assert `fields[3]` (rule_path) non-empty.
- `user/journal/parse.rs` tests: `parse_block_record`,
  `parse_block_interleaved_with_actions` → 4-field input + assert rule_path;
  `malformed_b_record_too_few_fields_skipped` → a 3-field B is now malformed.
- `user/cmd/journal.rs` / `user/cmd/review.rs` render tests: update Block
  constructions + assert the rule path appears.
- `user/journal/tree.rs`, `user/journal/core.rs` test constructions of
  `Note::Block` → add `rule_path`.

Tests expected to pass unchanged (use `{ path, .. }`): `block_record`-path
assertions, `denied_*`, `ro_write_emits_block`, `hidden_paths_do_not_log_block`
(static blocks still emit B; hide still emits nothing); `ask_*` A-note tests.

### Verification

- `make test-vm` (unit on host + e2e in VM). New/rewritten tests pass;
  existing tests still pass.
- Code review: parallel sub-agent review from `filesystem/AGENTS.md` (bugs,
  code quality, doc consistency, missing tests, plan adherence).
