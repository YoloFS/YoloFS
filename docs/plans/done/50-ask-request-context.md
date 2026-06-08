# Ask request context and decision/rule split

## Goal

Make permission prompts explain both the access being attempted and the rule
that caused the prompt, while keeping one-shot ask decisions distinct from
persistent rules.

The user-facing model should be:

- Ask **decisions** answer only the current blocked operation: `allow` or `deny`.
- Permission **rules** remain persistent path policy:
  `allow`, `write-ask`, `read-only`, `ask`, `deny`, `hide`.
- Ask requests sent to userspace carry enough context for `yolo watch` to say
  why the prompt exists, e.g. `rule: /etc asks before writes`.

## Current state

- `struct yolo_ioc_ask` returns one generic `path` using a userspace pointer,
  buffer capacity, and returned length.
- `struct yolo_perm_request` stores one generic `path`.
- Userspace `PermRequest` stores `path: String`.
- The daemon can infer the attempted operation (`read`/`write`) but not the
  effective rule source path.
- Ask decisions are still represented with the broader `Perm` enum in the
  userspace API and journal code, even though only `allow`/`deny` are meaningful
  as decisions.

## Target design

### Naming

Use explicit names in ask-related structs:

| Field | Meaning |
| --- | --- |
| `access_path` | The path being accessed, e.g. `/etc/hosts`. |
| `rule_path` | The path whose rule produced the effective permission, e.g. `/etc`; empty when the default root `ask` applies. |
| `rule_perm` | The permission stored on the source rule, or the default `ask` mode when `rule_path` is empty. |
| `op` | The operation being attempted (`read` or `write`). |

`target_path` is not currently used for ask requests, and `path` becomes
ambiguous once `rule_path` exists. Prefer `access_path` in userspace and the ABI.

Rename ask-specific structs for consistency:

| Current | New | Why |
| --- | --- | --- |
| `struct yolo_perm_request` | `struct yolo_ask` | Kernel-only in-flight ask state; "perm" is too broad and "request" is redundant in the ask engine. |
| `struct yolo_ioc_ask` | `struct yolo_ioc_ask` | Keep the ABI name; `ioc` already marks it as the ioctl payload. |
| `PermRequest` | `Ask` | Userspace-owned decoded ask. |

### ABI shape

Move the user/kernel ioctl ABI declarations out of `yolofs.h` and into
`kmod/yolofs_abi.h`. Avoid `ioctl.h` as a file name because it is easy to
confuse with Linux's ioctl headers and the implementation file `ioctl.c`.

Replace the pointer-based ask path output with fixed-size ABI buffers. Paths are
already capped at `YOLO_PATH_MAX`, and there is no backward compatibility
requirement, so fixed arrays are simpler than passing userspace pointers and
capacities.

```c
struct yolo_ioc_ask {
    __u64 id;
    __u32 op;
    __u32 pid;
    char comm[16];
    __u16 access_path_len;
    __u16 rule_path_len;
    __u8 rule_perm;
    __u8 _pad[3];
    char access_path[YOLO_PATH_MAX];
    char rule_path[YOLO_PATH_MAX];
};
```

The kernel fills the arrays and returns the actual byte lengths. Userspace reads
exactly those lengths instead of trusting NUL termination. `rule_path_len == 0`
means the default root `ask` policy, not an explicit rule path.

This makes `GET_ASK` larger by two fixed path arrays, but ask requests are the
slow interactive control path, not the filesystem data path. The extra copy is
negligible compared with blocking a task, waking `yolo watch`, and prompting a
human.

### Decision type

Introduce a separate ask-decision representation instead of reusing the full
rule enum:

```c
enum yolo_decision {
    YOLO_DECISION_DENY = 0,
    YOLO_DECISION_ALLOW = 1,
};
```

`YOLO_IOC_PUT_DECISION` accepts only this decision enum. The kernel maps it to
the current operation:

- `allow`: current read/write proceeds.
- `deny`: current read/write fails with `-EACCES`.

Persistent policy changes stay separate via `YOLO_IOC_RULE_SET`.

### Ask decision lifecycle

Ask decisions are one-shot. They never mutate the cached rule mode on the inode:

- For `ask`, `allow` lets the current read/write proceed and `deny` fails it.
  The inode remains effectively `ask`, so the next access asks again unless
  userspace separately installs a rule.
- For `write-ask`, `allow` lets the current write proceed and `deny` fails it.
  The inherited `write-ask` rule remains cached, so future writes ask again.

This removes the old implicit behavior where a broad permission-valued ask
answer could collapse a plain `ask` path into a cached future policy. Remembered
policy must be explicit through `YOLO_IOC_RULE_SET`.

### Journal

Update A-record decisions to match the two-decision model:

```text
A\0<access_path>\0<op>\0<decision>\n
```

where `decision` is:

- `y` = allow
- `d` = deny

Do not encode `ask`, `write-ask`, `read-only`, or `hide` as A-record decisions.
Those are rule modes, not decisions.

## Implementation steps

1. Update active docs (`docs/permissions.md`, `docs/staging.md`,
   `docs/architecture.md`, examples) to define `access_path`, `rule_path`,
   `rule_perm`, and the two-decision ask model.
2. Refactor kernel ask request storage and names:
   - Rename `struct yolo_perm_request` to `struct yolo_ask`.
   - Rename associated helpers such as `yolo_perm_request_release` to
     `yolo_ask_release`.
   - Rename generic request path storage to `access_path`.
   - Store `rule_perm`.
   - Store `rule_path` when resolution finds an explicit ancestor rule; use an
     empty rule path for the default root ask.
3. Refactor permission resolution:
   - Change the resolver to return both the effective `enum yolo_perm` and the
     source dentry/path for the rule that supplied it.
   - Stop storing the built-in default `ask` as a root dentry rule. Leave the
     root dentry at `UNSET`; if the resolver reaches the root without finding a
     rule, return `{ perm = ASK, rule_dentry = NULL }`.
   - An explicit `/ = ask` rule sets the root dentry's perm to `ASK`, so it
     returns `{ perm = ASK, rule_dentry = root }` and reports `rule_path = "/"`.
4. Move ioctl ABI enums, structs, constants, and `YOLO_IOC_*` macros to
   `kmod/yolofs_abi.h`; keep `yolofs.h` for kernel-only state.
5. Update `YOLO_IOC_GET_ASK` ABI to copy out `access_path` and `rule_path` via
   fixed-size `YOLO_PATH_MAX` arrays plus returned lengths.
6. Replace `YOLO_IOC_PUT_DECISION`'s permission-valued `decision` with a
   dedicated decision enum and map it to allow/deny for the current operation.
   Remove inode-cache mutation from ask decisions; remembered policy must go
   through `RULE_SET`.
7. Update userspace `ioctl.rs` and `watch.rs`:
   - Rename `PermRequest` to `Ask`.
   - Rename `path` to `access_path`.
   - Add `rule_path` and `rule_perm`.
   - Render prompt context like `rule: /etc asks before writes`.
   - Render the default-root case explicitly, e.g. `rule: default asks`.
   - Keep `parse_input` returning an explicit `Option`/decision type.
8. Move decision parsing/encoding off `Perm` and onto the new `Decision` type:
   - Rust journal parsing should reject every A-record decision except `y`/`d`.
   - Kernel A-record encoding should only accept `YOLO_DECISION_ALLOW` and
     `YOLO_DECISION_DENY`.
9. Update journal parser/formatter/tests for A decisions `y`/`d` only.
10. Add tests:
   - Unit tests for decision parsing and A-record parsing.
   - E2E tests that `write-ask` prompts include the inherited rule source.
   - E2E tests for default `ask` with no explicit `rule_path`.
   - E2E tests that an `allow` decision for a plain `ask` path is one-shot and
     does not cache future access.
   - Direct ioctl tests for invalid decision values.
11. Run validation (`make test-unit`, `make lint`, kernel build, VM e2e) and the
   required review categories, then move this plan to `docs/plans/done/`.
