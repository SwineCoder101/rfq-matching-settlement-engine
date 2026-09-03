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

## Change set 5

**Message:** `fix(engine): bound response deadlines at admission [REVIEW #1]`

**Colour:** turns both REVIEW #1 tests GREEN; default suite green, 0 ignored.

**Files:** `src/engine.rs`, `src/api.rs`, `tests/common/mod.rs`, `examples/demo.rs`,
`tests/failure_modes.rs`, `docs/FAILURE_MODES.md` (row P11), `ASSUMPTIONS.md`, this file.

**The defect:** `submit_quote` computed `response_deadline + accept_window` with chrono's
plain `+`, which panics on overflow. The panic unwound the actor task, so every later
command failed to send and the API answered 503 until restart. Admission had let the
deadline through because its only check was "in the future".

**What it does:** adds `EngineConfig::max_response_horizon` (default 365 days) and
`EngineError::DeadlineBeyondHorizon` → 400 `deadline_beyond_horizon`. `SubmitRequest`
now requires the deadline to be within the horizon and `deadline + accept_window` to be
representable, both via `checked_add_signed`. The later plain additions in `submit_quote`
and `tick` only ever see admitted values. The two REVIEW #1 tests drop their `#[ignore]`;
the second is reworded to assert refusal at the door and that the venue keeps answering,
because with a horizon a near-`MAX_UTC` deadline can no longer be admitted.

**Assumptions behind the fix:**
1. A year is an acceptable ceiling on how long maker collateral may sit reserved for one
   request. It is configuration, not a constant in the engine.
2. Validating once at admission is preferable to checked arithmetic at every later use:
   a stored request is guaranteed safe for the two sums the state machine performs on
   its deadline, and one guard is easier to defend than several.
3. The representability check stays even though the default horizon makes it
   unreachable, because the horizon is configurable and the failure it prevents is a
   venue-wide halt.
4. An error returned from the engine is always safe; only a panic can stop the actor.
   The fix therefore turns a panic into a returned `EngineError`, consistent with every
   other refusal.

**Verify:** `cargo test` (all green, 0 ignored), then
`cargo test --test failure_modes fm_far_future fm_response_deadline_beyond_horizon`.

## Change set 7

**Message:** `feat(request): tenor presets and resolves_at; accept window capped at resolution`

**Colour:** GREEN; new test `fm_accept_window_capped_at_resolution`.

**Files:** `src/domain/state.rs` (`Tenor`), `src/domain/mod.rs`, `src/domain/request.rs`,
`src/engine.rs`, `src/api.rs`, `examples/demo.rs`, `tests/common/mod.rs`,
`tests/happy_path.rs`, `tests/settlement.rs`, `tests/failure_modes.rs`, the three fixtures,
`ASSUMPTIONS.md`, `docs/ARCHITECTURE.md`, `docs/FAILURE_MODES.md` (row A13),
`docs/RESOLUTION.md`, this file.

**What it does:** a request body now requires `tenor`, one of `five_minutes`,
`ten_minutes`, `one_hour`, `one_day`. The engine stores it and `resolves_at =
response_deadline + tenor` (checked add, folded into the horizon check), and both appear in
the response. When a package is presented the accept deadline is
`min(now + accept_window, resolves_at)`. Sample contract descriptions are reworded to name a
strike instead of a date, since the tenor now fixes the instant.

**Assumptions behind it:** contracts are short-dated event markets that resolve at a fixed
offset from the quote window, so a preset tenor is the right shape and a free timestamp is
not; all legs of a request share the tenor, which is what makes one outcome per request
coherent; settlement is strike-based and the oracle applies the rule; the venue records the
instant but runs no timer on it, which stays the delay policy's job.

**Verify:** `cargo test`, `cargo run --example demo`.

## Change set 8

**Message:** `test(fm): dispute window with delayed finality [RESOLUTION design] [red]`

**Colour on first run:** RED (all seven), ignored in the default suite with
`#[ignore = "dispute window: implementation pending"]`.

**Files:** `tests/failure_modes.rs` (new section at the end), `tests/common/mod.rs`
(`DISPUTE_WINDOW_SECS`, `UNWIND_TIMEOUT_SECS`, `dispute()` helper), this file.

**Tests and what each pins:**
- `fm_report_does_not_pay_out`: `yes` from Locked → state `reported`, `reported_outcome`,
  `dispute_deadline = now + dispute_window`, escrow untouched.
- `fm_unfiled_report_settles_after_window`: still `reported` at the deadline instant; first
  tick past it settles the reported outcome once; a later filing is 409.
- `fm_stranger_cannot_dispute`: a stranger and a maker whose quote lost are 403 `not_owner`.
- `fm_party_dispute_holds_escrow`: requester or a locked maker files → `disputed`,
  `unwind_deadline = now + unwind_timeout`, nothing moves, the old window no longer settles
  it, a second filing is 409.
- `fm_adjudication_settles_or_unwinds_once`: from Disputed, `no` pays the Yes-sellers and
  `invalid` refunds each poster, immediately and once; any further resolve or filing is 409.
- `fm_unwind_timeout_refunds_each_poster`: still `disputed` at the unwind instant; first tick
  past it refunds each poster once; later resolve is 409.
- `fm_dispute_only_while_reported`: filing while Open, Presented, or Locked is 409; a second
  resolve while Reported is 409 and the report is unchanged.

**Why red today:** `POST /v1/requests/{id}/dispute` does not exist (404), and `yes` / `no`
from Locked pays out in the same command and goes straight to `settled`.

**Verify:** `cargo test` (green, 7 ignored) and
`cargo test --test failure_modes -- --ignored fm_` (7 failed, expected).

## Change set 9

**Message:** `feat(engine): dispute window with delayed finality; failure-mode tests grouped by state`

**Status:** implemented; all seven dispute tests green, suite green with 0 ignored.

**Also in this set:** `tests/failure_modes.rs` split into `tests/failure_modes/{main,scenarios,open,presented,locked,reported,disputed}.rs`, one module per request state, and the three-leg baseline moved to `tests/happy_path.rs` as `three_legs_three_makers_settle_yes`. Other test names are unchanged, so the FAILURE_MODES rows still resolve.

**Shape:**
- `RequestState::Reported`; `RfqRequest` gains `reported_outcome: Option<OracleOutcome>`,
  `dispute_deadline: Option<DateTime<Utc>>`, `unwind_deadline: Option<DateTime<Utc>>`, all
  skipped when absent.
- `EngineConfig` gains `dispute_window` and `unwind_timeout`; the harness passes 60 s and
  600 s (`tests/common/mod.rs` constants), the demo and `main` use defaults.
- `Resolve` from Locked with `yes` / `no` → Reported, no ledger call. `invalid` from Locked
  → Unwound as today. `disputed` from Locked → Disputed with an unwind deadline (oracle-
  initiated dispute, keeps R2 and R4 valid). Any resolve while Reported → 409.
- New `Command::Dispute { party, request_id }` and `POST /v1/requests/{id}/dispute` with
  the `Party` extractor: state must be Reported (409), party must be the requester or the
  maker of a `Locked` quote (403 `not_owner`); → Disputed, `unwind_deadline` set, no ledger
  call.
- From Disputed, `yes` / `no` settles and `invalid` unwinds immediately (as today).
- `Tick`: Reported past `dispute_deadline` (`now > deadline`) → settle the reported outcome;
  Disputed past `unwind_deadline` → refund each poster. Both use the existing per-leg
  payout / refund loops, so money still moves exactly once per leg.
- Boundaries match the accept window: allowed at the deadline instant, closed after it.

**Existing tests to update in the same commit** (they assume `yes` / `no` from Locked pays
out immediately): `fm_happy_path_three_legs`, `fm_accept_after_terminal_is_409`,
`full_lifecycle_two_legs_settles_yes`, the `settles` matrix in `tests/settlement.rs`, and
`examples/demo.rs`. Each becomes resolve, then `advance_to` past the dispute window, then
the same balance assertions. `fm_disputed_then_yes_pays_out`, `fm_disputed_exits`,
`fm_resolve_invalid_unwinds_refunds_each_side`, and `fm_resolve_before_locked_is_409` are
unaffected.

**Docs in the same commit:** `docs/RESOLUTION.md` "Disputed" becomes the implemented
mechanism (drop the "designed, not implemented" framing, keep the bond as the open cost);
`docs/ARCHITECTURE.md` state diagram gains `Reported` and the `dispute` route;
`docs/FAILURE_MODES.md` gains one row per test above (D1–D7) and drops the party-dispute
known-gap line; `ASSUMPTIONS.md` gains one line on delayed finality and free filing.

## Not in this plan

- X1 (self-quote): its test is removed and the behaviour is documented as allowed and
  harmless in `ASSUMPTIONS.md` and the known-gaps list. Refusing it would be an identity
  rule, which the brief puts out of scope. The change lands with change set 5.

- FAILURE_MODES rows for change sets 2 and 3 (A12, R4) still to be added with a fix or
  doc commit of your choosing.
