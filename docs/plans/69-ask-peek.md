# 69 — Non-consuming ask read (ASK_PEEK), single queue

## Motivation

Asks are handled one at a time: the daemon reads the next ask, decides, and
answers it before reading the next. The kernel nonetheless kept a two-list
protocol — `pending_reqs` (not yet handed out) and `dispatched` (handed to the
daemon, awaiting a decision) — plus a per-dispatch second `kref` and a
`dispatched` flag. That machinery only existed because `GET_ASK` *consumed*
(dequeued) the ask, so a timed-out/killed requester racing the daemon's answer
needed a second reference to keep the req alive.

Plan 68 then added a `.poll` fop purely so a caller could detect a *pending*
(not-yet-dispatched) ask non-destructively — a distinction that only matters
because `GET_ASK` consumed.

If reading an ask does **not** consume it, all of that collapses.

## Design

- **Single FIFO.** One `pending_reqs` list. Drop `perm->dispatched`, the
  `yolo_ask.dispatched` flag, and the per-dispatch `kref_get`.
- **`ASK_PEEK` (was `GET_ASK`).** Non-consuming: copy the head req to the
  caller under `pending_lock`, unlock, `copy_to_user`. The req stays on the
  queue. Blocking (waits for a non-empty queue) unless `O_NONBLOCK`. Still
  claims the daemon slot on first call.
- **`ASK_DECIDE`.** Looks the req up by id in `pending_reqs`, sets the
  decision, removes it, and completes — this is the only op that removes an
  answered ask. Returns `ENOENT` if the id is gone (the requester already
  timed out or was killed).
- **Ownership.** The requesting thread holds the req's single ref for its
  whole life. No other context frees it.
- **Correctness rule (replaces the second ref).** Every `complete(&req->done)`
  runs under `pending_lock`, and the requester always re-acquires
  `pending_lock` after waking (it already does, to settle). So an answer can
  never race the requester's free: whoever completes finishes touching the req
  under the lock before the requester can proceed past its own lock
  acquisition. A requester that removed the req itself (timeout/kill) is never
  seen by `ASK_DECIDE`, which only touches reqs found on the list under the
  lock.
- **Drop `.poll`.** The single-purpose daemon uses a blocking `ASK_PEEK`;
  tests detect a pending ask with a non-blocking `ASK_PEEK` (non-consuming, so
  it leaves the ask queued). Nothing needs `poll`/`epoll` multiplexing.

## Userspace

- `ioctl::get_ask` → `ioctl::ask_peek`.
- `claim_daemon` no longer has to answer an ask that raced the claim — a
  non-blocking `ASK_PEEK` leaves any queued ask in place for the main loop, so
  it just claims the slot.
- The watch loop tolerates `ENOENT` from `ASK_DECIDE`: a peeked ask can time
  out before the user answers it.

## Out of scope

Out-of-order / concurrent answering of multiple in-flight asks (the old
`dispatched` list technically allowed it) is dropped — the daemon is
serial, so it was never used.
