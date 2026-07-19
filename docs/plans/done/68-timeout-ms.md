# 68 — Millisecond prompt timeout + faster permission tests

## Motivation

The e2e suite is fast overall (~7s for 590 tests), but a few tests dominate:

- `cli::test_unmount::unload_retries_until_module_quiesces` — ~2.6s. A 10×
  loop with a 200ms sleep per cycle trying to reproduce a `delete_module`
  race that (per its own comment) never reproduced in ~90 cycles. The retry
  logic it targets is already covered by `retry_busy` unit tests.
- `cli::test_watch::{dispatched_ask_times_out_to_deny,
  pending_ask_times_out_and_is_removed}` — ~1.07s each. They wait on the
  kernel `ask` timeout, whose minimum non-zero value is **1 second** because
  the `prompt_timeout` mount option is `u32` seconds.
- Three daemon-readiness tests — ~0.21s each, entirely from a fixed 200ms
  `sleep` used to wait for the daemon to block on the ioctl read.

## Changes

1. **Timeout granularity → milliseconds.**
   - Kernel: rename mount option `prompt_timeout` → `prompt_timeout_ms`
     (still `fsparam_u32`), field `timeout_s` → `timeout_ms`. The jiffies
     conversion drops the `* 1000`.
   - Userspace `Config.prompt_timeout` becomes `Option<f64>` **seconds** (the
     user-facing unit is unchanged; `30` still means 30s, and fractional
     values like `0.1` are now allowed). `mount_options` emits
     `prompt_timeout_ms = round(secs * 1000)`.
   - Tests set `prompt_timeout: Some(0.1)` (100ms) instead of `Some(1)`.

2. **Drop** `unload_retries_until_module_quiesces`. Retry behavior stays
   covered by the `retry_busy` unit tests in `user/cmd/load.rs`.

3. **Replace daemon-readiness sleeps with polling.**
   - `spawn_watch_with_input` waits on the daemon's stderr readiness line
     instead of sleeping 200ms.
   - `daemon_close_denies_pending_ask` waits for the ask to appear instead of
     sleeping 200ms. (This originally added a `.poll` fop for non-destructive
     detection; plan 69 makes the ask *read* itself non-consuming, so the
     test uses a non-blocking `PEEK_ASK` and `.poll` was removed again.)

## Out of scope

Real users never need sub-second timeouts; f64 seconds is purely to let the
config express the small values the tests want without a second unit.
