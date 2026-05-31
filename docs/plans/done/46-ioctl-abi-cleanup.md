# 46 — ioctl/ABI naming cleanup (three commits)

Make the kernel↔userspace ABI consistent with the snapshot/travel/permission
vocabulary, and make `yolo rule resolve` authoritative (kernel-resolved). No
backward compatibility; on-disk format may change.

## Commit 1 — Rules: `RULE_SET` + `RULE_RESOLVE`

- Merge `RULE_ADD`(10)+`RULE_REMOVE`(11) → **`YOLO_IOC_RULE_SET`** `_IOW('A',10)`.
  `perm == UNSET` clears the rule (unpin + reset); else sets it. One handler.
- Add **`YOLO_IOC_RULE_RESOLVE`** `_IOWR('A',11, struct yolo_ioc_rule)`:
  userspace passes the path, kernel runs `yolo_resolve_perm` on the resolved
  dentry and writes the effective perm into the struct's `perm` field (OUT).
- userspace `ioctl.rs`: `add_rule`/`remove_rule` → `set_rule(fd,path,perm)` +
  `resolve_rule(fd,path) -> Perm`.
- `config.rs`: `set_rule`/`unset_rule` use `RULE_SET`. `show_rule` (CLI
  `rule resolve`) — when mounted, take the **kernel** perm as authoritative and
  annotate the source from the toml resolver; warn if they diverge. When
  unmounted, fall back to the userspace toml resolver alone.
- CLI: `rule show` → **`rule resolve`** (verb matches ioctl + `yolo_resolve_perm`).
- e2e parity test: set rules, assert `rule resolve` matches observed enforcement.

## Commit 2 — Ask wire structs + wrapper alignment

- `struct yolo_ctl_request`/`yolo_ctl_response` → **`yolo_ioc_ask_request`/
  `yolo_ioc_ask_response`** (all ioctl payloads share the `yolo_ioc_` prefix;
  adds the "ask" domain word). In-kernel `yolo_perm_request` unchanged.
- ioctl command names `GET_REQUEST`/`PUT_RESPONSE` unchanged (verbs = actions).
- userspace wrappers `read_request`/`write_response` → **`get_request`/
  `put_response`** to match the ioctls they issue.

## Commit 3 — Snapshot / Travel (incl. on-disk tags)

- ioctl `MARK`(40)→**`SNAPSHOT`**, `JUMP`(41)→**`TRAVEL`**; structs
  `yolo_ioc_mark`→`yolo_ioc_snapshot`, `yolo_ioc_jump`→`yolo_ioc_travel`.
- Constants: `YOLO_MARK_IF_CHANGED`→`YOLO_SNAPSHOT_IF_CHANGED`,
  `YOLO_JUMP_MAX_DEPTH`/`YOLO_JUMP_MAX_TREE_LEN`→`YOLO_TRAVEL_*`.
- Journal: `Meta::Mark`→`Meta::Snapshot`, `Meta::Jump`→`Meta::Travel`; comments
  ("M/J skeleton" etc.). **On-disk record tag bytes `'M'`→`'P'`, `'J'`→`'T'`**
  (matches the paper; `P`/`T` avoid the `S`/`D`/`R`/`B` collision). Update
  `journal.c` writers and `parse.rs` readers together.
- userspace wrappers `mark`→`snapshot`, `jump`→`travel`; Rust structs likewise.
- `abort` stays the CLI command but calls `travel(0)` (travel-to-initial).

## Verification (each commit)

`cargo build --tests`, `cargo test --lib`, `make kmod`, fmt, review protocol,
`make test-vm` for the perm/cli/internals suites (needs the VM).
