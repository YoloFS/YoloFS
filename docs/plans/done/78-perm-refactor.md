# 78 — Permission layer reorg & refactor

Three cohesion/clarity cleanups to the permission subsystem. No behavior
change; pure refactor. `make test` must stay green.

## 1. Thread `enum yolo_op` through perm.c instead of `int f_flags`

`yolo_perm_check_dentry` takes `int f_flags`, but every internal path collapses
it to read-vs-write immediately. Only the `yolo_open` caller has real open
flags; the metadata/setattr callers pass a synthetic `O_WRONLY` purely to mean
"this is a write", which reads misleadingly.

- Change the public signature to
  `yolo_perm_check_dentry(sbi, check, target, enum yolo_op op)`.
- Convert the internal helpers to take `enum yolo_op op`:
  `yolo_perm_check`, `yolo_perm_check_static`. Drop the now-unused `f_flags`
  param from `yolo_perm_ask` (it already only used `op`).
- Move `yolo_open_op(int f_flags)` (VFS open-flags → op) out of perm.c and into
  file.c next to its only real caller, as a `static` helper.
- Call sites:
  - `file.c` (`yolo_open`): `yolo_perm_check_dentry(sbi, dentry, dentry,
    yolo_open_op(file->f_flags))`.
  - `inode.c` (`yolo_check_mutate_perm`, `yolo_setattr`): pass
    `YOLO_OP_WRITE` directly (self-documenting, replaces `O_WRONLY`).

## 2. Co-locate the ask protocol in perm.c

The ask lifecycle is split: requester (`yolo_ask_userspace`) in perm.c, the
`ASK_PEEK` / `ASK_DECIDE` responder handlers in ioctl.c. The subtle
"unlink-under-lock means resolved" invariant is commented in both files. Move
both responder handlers into perm.c so the whole lifecycle and its locking
discipline live in one file.

- Move `yolo_ask_peek_ioctl` and `yolo_ask_decide_ioctl` from ioctl.c to perm.c;
  declare them in the perm.c section of `yolofs.h` (non-static).
- ioctl.c dispatch (`yolo_ctl_ioctl`) keeps routing `YOLO_IOC_ASK_PEEK` /
  `YOLO_IOC_ASK_DECIDE` to them; `yolo_caller_inside` gating stays in ioctl.c.
- Add `#include <linux/uaccess.h>` to perm.c (copy_to/from_user).
- Trim the now-redundant ASK detail from ioctl.c's file header comment.

## 3. Extract `yolo_ask_settle` helper

The "find req by id → set decision → unlink → complete, under pending_lock"
sequence is duplicated in the decide handler and in peek's EFAULT-recovery path.
Extract:

```c
static bool yolo_ask_settle(struct yolo_permission *perm, u64 id,
                            enum yolo_decision decision);
```

Returns whether the ask was still pending. `ASK_DECIDE` returns
`found ? 0 : -ENOENT`; peek's EFAULT path calls it with `YOLO_DECISION_DENY`.

## Verification

- `make test` (unit + e2e). Existing `tests/perm/` covers allow/deny/ask/
  write-ask/read-only and timeout behavior — no behavior change expected.
- Code review per AGENTS.md before finalizing.
