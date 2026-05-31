# 44 — Rename permission levels to all-verbs

## Goal

Make the `yolo rule` permission levels a consistent set of verbs:

- `ro` → `read`
- `hidden` → `hide`
- `ask`, `allow`, `deny` unchanged.

No backward compatibility (no external users). The on-disk numeric values
(`YOLO_PERM_* = 1..5`) are unchanged; only the names/strings change.

## Token mapping (targeted — `ro` is NOT swept as a substring)

- Identifiers: `YOLO_PERM_RO`→`YOLO_PERM_READ`, `YOLO_PERM_HIDDEN`→`YOLO_PERM_HIDE`
  (user/ioctl.rs, kmod/*.{c,h}); `Perm::Ro`→`Perm::Read`, `Perm::Hidden`→`Perm::Hide`
  and the enum variants `Ro`→`Read`, `Hidden`→`Hide` (user/config.rs).
- Rule-value strings: `"ro"`→`"read"`, `"hidden"`→`"hide"` in Display/FromStr
  (config.rs), `parse_input` (watch.rs, dropping the `ro` alias; keeping
  `read-only`/`readonly`), the `yolofs.toml` template, and rule/command
  examples in docs.
- main.rs `rule add` help: `allow, ro, deny, hidden, ask` → `allow, read, deny, hide, ask`.

## Keep as-is (descriptive prose, not the level value)

- "read-only", "read + execute", "invisible" descriptions.
- kmod local `bool is_hidden` and comments like "skip — hidden" (correct
  English describing the *state*, parallel to keeping "read-only" prose).

## Verification

- `cargo build --tests`, `cargo test --lib`, `make kmod`.
- Re-read docs for value vs prose correctness.
- `make test-vm` for the perm e2e suite (needs the VM).
