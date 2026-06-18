# Working under YoloFS

This project runs your shell commands through **YoloFS**. A pre-tool-use hook
wraps every command as `yolo run -- <cmd>`, so each command's filesystem effects
are **staged**, not written to the real project. Work normally — YoloFS prints
what changed after each command. A human reviews the staged result at the end
and decides whether to keep or discard it. You cannot damage the real tree.

## Inspect what you've changed

- `yolo review` — summary of all staged changes since the base.
- `yolo review --diff` — the staged changes as a git-style diff.
- `yolo journal -- <path>` — the operation history for one path.
- `yolo timeline` — the snapshot/travel graph.

## Recover from a mistake

If a command corrupted or deleted files — a bad script, an over-aggressive
clean, a failed build — **rewind; do not hand-restore.** Re-typing the old
contents or reaching for `git` will not reproduce the originals (they may differ
byte-for-byte, and there may be no git history). YoloFS auto-snapshots before
every command, so the exact originals are one rewind away:

- `yolo timeline` — show the snapshots; find the generation to return to (the
  `initial` snapshot, or the one just before the command that did the damage).
- `yolo travel <gen>` — rewind the working tree to it, e.g. `yolo travel initial`
  (or `yolo travel 0` for the base). This restores the files exactly. The
  abandoned branch stays visible in `yolo timeline`.
- `yolo snapshot <name>` — optionally bookmark a good state before something
  risky; the name is just a label shown in `yolo timeline`.

When something looks wrong, reach for `yolo travel` first — it is faster and
exact, where hand-repair is slow and error-prone.

## Leave the result for a human

When the staged changes are correct, **stop and report that they are ready for
review.** Finalizing is not yours to do:

- Do **not** run `yolo commit`, `yolo abort`, or `yolo rule`. Applying changes,
  discarding them, and changing permissions are the human's decisions — these
  commands are blocked for you anyway.

You may run only these YoloFS subcommands: `review`, `journal`, `timeline`, `travel`, `snapshot`.
