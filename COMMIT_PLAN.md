# Commit plan — test-first pass over REVIEW.md findings 1, 3, 7, 8

Nothing here is committed or staged by the pass that wrote it. Stage each change set
yourself (by hunk where a file carries more than one set), run `cargo test`, and commit
with the proposed message. Every commit point below leaves the default suite green; the
only red tests are `#[ignore]`d and run via `-- --ignored`.

Files touched by the whole pass: `tests/failure_modes.rs`, `tests/happy_path.rs`,
`tests/settlement.rs`, and this file. `src/`, `docs/`, `README.md`, `ASSUMPTIONS.md`,
`REVIEW.md` are untouched.

Pre-existing, unrelated working-tree state (from earlier passes, not part of this plan):
the `src/` restructure, doc rewrites, `examples/demo.rs`, `tests/common/mod.rs`, and the
already-staged deletions of `tests/parlay.rs` and three unused fixtures.

---

## Change set 1

**Message:** `test(fm): far-future deadline must not take the venue down [REVIEW #1] [red]`

**Colour on first run:** RED (both), by design; ignored in the default suite.

**File:** `tests/failure_modes.rs`

**Hunks to stage:**
- line 11: `use chrono::{DateTime, Duration, Utc};` (new import; needed by this set only)
- `fm_response_deadline_beyond_horizon_rejected` (starts line 978, with its doc comment
  and `#[ignore = "REVIEW #1: fix pending"]`)
- `fm_far_future_deadline_cannot_kill_engine` (starts line 1002, same attribute)

**Pins:** a deadline centuries out is refused with 400 and nothing stored; a deadline
admitted near `MAX_UTC` followed by one quote leaves the venue able to serve an unrelated
party (credit 200, open 201).

**Verify:** `cargo test` (green, 3 ignored) and
`cargo test --test failure_modes -- --ignored fm_response_deadline_beyond_horizon fm_far_future`
(2 failed, expected until the fix lands). When the fix lands, drop the two `#[ignore]`
attributes in the fix commit and add the FAILURE_MODES rows there.

## Change set 2

**Message:** `test(fm): accept re-checks the deadline without a tick [REVIEW #3] [green]`

**Colour on first run:** GREEN.

**File:** `tests/failure_modes.rs`

**Hunks to stage:**
- `fm_accept_past_deadline_without_tick_fails_request` (starts line 339)

**Pins:** with the clock one second past `accept_deadline` and no `Tick` delivered, accept
returns 409 `wrong_state`, the request is `failed(accept_window_expired)`, every quote is
`released`, every maker is back to `bal(SIDE_LOCK, 0, 0)`, the requester still holds
`3 * SIDE_LOCK` free, no lock batch was attempted, and the ledger conserves. With the clock
exactly at `accept_deadline`, accept succeeds and locks `3 * SIDE_LOCK`.

**Verify:** `cargo test --test failure_modes fm_accept_past_deadline`.

## Change set 3

**Message:** `test(fm): every exit from Disputed [REVIEW #7] [green]`

**Colour on first run:** GREEN.

**File:** `tests/failure_modes.rs`

**Hunks to stage:**
- line 14: `ThreeLeg` added to the `use common::{...}` list (needed by this set only)
- `fm_disputed_exits` (starts line 1124)

**Pins:** from `Disputed`: a repeated `disputed` is 200 with state and balances unchanged;
`no` settles and pays each maker (the Yes-seller on a `buy_yes` leg) `LEG_NOTIONAL` while
the requester ends at zero; `invalid` unwinds, returning `3 * SIDE_LOCK` to the requester
and `SIDE_LOCK` to each maker, and a later `yes` is 409. Conservation after each.

**Verify:** `cargo test --test failure_modes fm_disputed_exits`.

## Change set 4

**Message:** `test: venue-escrow identity in happy path and settlement [REVIEW #8] [green]`

**Colour on first run:** GREEN.

**Files:** `tests/happy_path.rs`, `tests/settlement.rs`

**Hunks to stage:**
- `tests/happy_path.rs:180` — `v.assert_conserved().await;` at the end of
  `full_lifecycle_two_legs_settles_yes`
- `tests/happy_path.rs:230` — same, at the end of
  `unmatched_leg_fails_request_and_releases_every_reservation`
- `tests/settlement.rs:161` — same, at the end of `settles` (all 14 cases)
- `missing_party_header_is_unauthorized` deliberately left alone (funds no one)

**Pins:** the per-party conservation identity and the venue-wide escrow identity
(`escrowed == Σ notionals of Locked/Disputed requests`) hold at the end of every
money-moving scenario in these two files, not only in `failure_modes.rs`.

**Verify:** `cargo test --test happy_path --test settlement`.

---

## Not in this plan

- X1 (`fm_self_quote_rejected`): the red test already exists and stays `#[ignore]`d; no
  second test was written.
- No `src/` change, no doc change. FAILURE_MODES rows for the four new tests arrive with
  the fix commits, once each test's colour is final.
