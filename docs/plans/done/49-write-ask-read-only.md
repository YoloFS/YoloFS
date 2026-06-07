# Write-ask and read-only permission modes

## Goal

Replace the overloaded `read` permission name with explicit user-facing modes:

- `write-ask`: reads are allowed; writes prompt.
- `read-only`: reads are allowed; writes are denied.

Keep the internal representation as a closed enum, not an arbitrary read/write
matrix. The default unmatched policy remains `ask`, so unknown paths still prompt
before reads and writes.

## Design

The user-facing policy set is:

| Rule | Read | Write | Visibility |
| --- | --- | --- | --- |
| `allow` | allow | allow | visible |
| `write-ask` | allow | ask | visible |
| `read-only` | allow | deny | visible |
| `ask` | ask | ask | visible |
| `deny` | deny | deny | visible |
| `hide` | deny | deny | hidden |

Directory rules stay subtree/default rules: the nearest ancestor rule applies to
the path being opened or to the parent directory for metadata mutations. `hide`
remains the visibility exception and makes the path look absent.

`write-ask` asks only for write operations. If the daemon answers a write ask,
that answer applies to the current operation; the cached inherited
`write-ask` policy should remain so later writes ask again. For a plain `ask`
path, keep the existing behavior of caching the daemon's chosen policy until the
next permission generation invalidation.

## Implementation steps

1. Update docs and examples to document `write-ask` and `read-only`.
2. Add `YOLO_PERM_WRITE_ASK` and `YOLO_PERM_READ_ONLY` to the kernel/userspace
   ABI and update journal letter mapping (`w` and `r` respectively).
3. Refactor Rust `Perm` to `WriteAsk` and `ReadOnly`, update CLI rule verbs to
   `write-ask` and `read-only`, and update config/journal parsing tests.
4. Update kernel permission checks so `write-ask` asks for writes, allows reads,
   and does not collapse into `allow` after one approved write.
5. Update permission/CLI/watch tests for the new names and add write-ask
   read/write coverage.
6. Run the required verification and code review, then move this plan to
   `docs/plans/done/`.
