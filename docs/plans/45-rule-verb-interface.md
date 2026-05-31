# 45 — Verb-based `yolo rule` interface

## Goal

Redesign `yolo rule` around the permission states plus two queries, and rename
the sentinel constant to match.

### CLI

```
yolo rule {unset, ask, allow, read, deny, hide} <path>   # set/clear a rule
yolo rule list                                            # list configured rules
yolo rule show <path>                                     # effective level + source
yolo rule                                                 # bare → same as `list`
```

Replaces the old `yolo rule add <path> <perm>` / `yolo rule remove <path>`.
Every mutating verb names a permission state (`unset` clears the rule, i.e. sets
it back to `YOLO_PERM_UNSET`); `list`/`show` are read-only queries.

### Constant rename

`YOLO_PERM_NONE` → `YOLO_PERM_UNSET` (user/ioctl.rs + kmod). "No rule on this
dentry" — `NONE` reads like a deny sibling; `UNSET` says "no rule, inherit".
Numeric value (0) and wire format unchanged. No Rust `Perm` variant (UNSET is
only the kernel sentinel; the `unset` verb maps to the existing RULE_REMOVE).

## Implementation

- `user/ioctl.rs`, `kmod/*`: `YOLO_PERM_NONE`→`YOLO_PERM_UNSET`.
- `user/config.rs`: `add_rule(path, perm_str)` → `set_rule(path, Perm)` (the verb
  supplies the level, no string parse); `remove_rule` → `unset_rule`; new
  `list_rules()` and `show_rule(path)`. `show` resolves the longest ancestor rule
  over the persisted `[rules]` (lexical normalize: expand `$HOME`/`~`, make
  absolute vs cwd) and reports `explicit` vs `inherited from <path>`, else the
  `ask` default.
- `user/main.rs`: restructure `RuleAction` into the verb subcommands; `action`
  becomes `Option` so bare `yolo rule` lists.
- Docs: cli.md, permissions.md, architecture.md.
- Tests: update test_rules.rs CLI call; add unit tests for `show_rule`
  resolution (explicit / inherited / default) and `list_rules`.

## Verification

`cargo build --tests`, `cargo test --lib`, `make kmod`, review protocol,
`make test-vm` (perm/cli suites need the VM).
