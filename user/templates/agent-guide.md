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

## Undo a mistake

Every command auto-snapshots, so you can rewind instead of trying to hand-repair
a bad state:

- `yolo snapshot <name>` — bookmark the current state before something risky.
  The name is just a label shown in `yolo timeline`.
- `yolo travel <gen>` — rewind the working tree to an earlier snapshot by its
  generation id (e.g. `yolo travel 3`, or `yolo travel 0` for the initial
  state). Run `yolo timeline` to see the generation ids. The abandoned branch
  stays visible in `yolo timeline`.

## Leave the result for a human

When the staged changes are correct, **stop and report that they are ready for
review.** Finalizing is not yours to do:

- Do **not** run `yolo commit`, `yolo abort`, or `yolo rule`. Applying changes,
  discarding them, and changing permissions are the human's decisions — these
  commands are blocked for you anyway.

You may run only these YoloFS subcommands: `review`, `journal`, `timeline`, `travel`, `snapshot`.
