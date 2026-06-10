# 55 — Retry `delete_module` on EAGAIN in `yolo unload`

## Problem

`yolo unload` intermittently fails with `delete_module (unloading yolofs):
Resource temporarily unavailable` even though every session was just
unmounted. `umount(2)` only guarantees the mount is detached from the
namespace; the superblock teardown that releases the module's refcount runs
at the *last* reference drop, which can happen asynchronously in another
context (e.g. a killed or exiting process's deferred `fput` running on a
kernel workqueue). `unload()` calls `delete_module(2)` microseconds after
`umount(2)`, inside that window. The old `sudo umount; sudo rmmod` flow
masked the race with two fork/exec/sudo round-trips of accidental latency.

There is no syscall-level "wait until free": the kernel removed blocking
`delete_module` in 3.13. A bounded userspace retry is the standard idiom
(the kernel's own selftests wrap `rmmod` this way).

## Design

In `load.rs::unload()`, retry `delete_module(2)` on `EAGAIN`/`EBUSY` with a
short sleep (50ms) up to a ~2s deadline. Success on any attempt is silent —
no extra output on the happy path. If the deadline expires, fail with the
current error, extended with the live count from
`/sys/module/yolofs/refcnt` (e.g. `module still has 1 reference after 2s`)
so the message says what is wrong instead of a raw errno.

Factor the loop as a small `retry_busy(deadline, step, f)` helper in
`load.rs` so the policy is unit-testable with a closure (no kernel needed).

Nothing else changes: `O_NONBLOCK` stays (a blocking call would hang
forever on a genuinely held reference), and no force-unload.

## Steps

1. Docs: one sentence in `docs/cli.md` under the unload command: unload
   briefly waits for the module to quiesce after unmounting.
2. Failing test first: e2e test in `tests/cli/test_unmount.rs` — spawn a
   child holding an open file inside the mount, SIGKILL it, and immediately
   `yolo unload`; loop the cycle a few times to widen the race window.
   Verify it reproduces the EAGAIN on the VM before fixing (it is a race —
   if it won't reproduce reliably, note that and keep the test as a
   regression smoke test).
3. Implement `retry_busy` + the retry in `unload()`, with inline unit tests
   for the helper (retries on busy, stops on success, gives up at the
   deadline).
4. `make test-vm`.
5. Code review per AGENTS.md.

## Outcome notes

Step 2: the race did not reproduce on the test VM — ~90 kill→unload cycles
all passed before the fix was applied. The e2e test is kept as a regression
smoke test (`unload_retries_until_module_quiesces`), as anticipated.

## Non-goals

- No force unload (`O_TRUNC`) — unsafe.
- No change to `umount_or_prompt`'s kill-and-wait (the fixed 200ms sleep);
  the retry covers deferred teardown regardless of its source. Revisit only
  if EAGAIN persists past the retry in practice.
- No retry anywhere else (`mount`, `umount` keep their current behavior).
