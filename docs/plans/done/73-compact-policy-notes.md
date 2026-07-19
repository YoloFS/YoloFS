# 73 — Compact Policy Notes and Complete Review Output

Status: complete.

## Goal

Simplify the G/C journal follow-up design:

- encode C policies as one byte instead of full policy names;
- show every G and C note selected by `yolo review`, without deduplication;
- inline the simple r/w operation encoding in the kernel G writer.

No backward compatibility is required. C records containing full policy names
will no longer parse.

## Format

The C shape remains `C\0<path>\0<policy>\n`. Policy codes are:

| Code | Policy |
|------|--------|
| `q` | ask |
| `a` | allow |
| `w` | write-ask |
| `r` | read-only |
| `d` | deny |
| `h` | hide |
| `u` | unset |

The CLI expands these codes back to their full policy names for display.

## Changes

1. Update staging and permission documentation with the compact codes and state
   that review preserves every selected note in journal order.
2. Replace the userspace policy parser with strict one-byte conversion and
   update parser/format tests.
3. Replace the kernel policy-name mapping with a policy-code mapping.
4. Remove G deduplication and its note-dedup set from `Changeset::collect`;
   retain normal reachability filtering for G and non-branching behavior for C.
5. Inline `op == YOLO_OP_WRITE ? 'w' : 'r'` in the G writer.
6. Run the full test suite and required reviews, then archive this plan.
