# Failure modes

The engine actor serialises every mutation; the ledger is reserve (quote-scoped) then escrow (request-scoped, one `lock_batch` at accept). Every row is pinned by the named test under `tests/failure_modes/`, one module per request state (`open`, `presented`, `locked`, `reported`, `disputed`), and every test there is a row here. Each test asserts HTTP status, request and quote states, numeric balances, and ledger conservation where they apply. The baseline H1 lives with the other happy paths in `tests/happy_path.rs`.

Rows are grouped by the request state they exercise, in the same order as the test modules, so each section reads against one file. The letter in a row id says what kind of thing could go wrong (**H** happy baseline, **M** multi-leg atomicity, **A** accept window and timing, **P** party misbehaviour or bad input, **R** resolution, **D** dispute window); the number is only its order within that letter.

## Happy baseline

`tests/happy_path.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| H1 | Baseline: three legs, three makers, accepted, reported Yes, window closes | — | Reserve at quote, one `lock_batch` at accept, report held for `dispute_window`, then `payout` of `n` per leg to the Yes-buyer | `three_legs_three_makers_settle_yes` |

## Open: admission and quoting

`tests/failure_modes/open.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| P7 | No identity header | Anonymous mutation | `x-party-id` extractor → 401 before any handler runs | `fm_missing_party_header_is_401` |
| P8 | Unknown request, leg, or quote id | — | 404, no side effects | `fm_unknown_ids_are_404` |
| P9 | Price 0 or 100 %, zero notional, empty legs, blank contract | Untradeable leg | Parsed into newtypes at the boundary → 400 | `fm_invalid_body_rejected` |
| P10 | Response deadline in the past | Request that can never present | `response_deadline ≤ now` → 400 | `fm_response_deadline_in_past_rejected` |
| P11 | Response deadline beyond the venue horizon, or so far out that `deadline + accept_window` is not representable | Maker collateral reserved for years; a deadline sum that panics the actor and turns every endpoint into 503 | Both checked at open: `deadline ≤ now + max_response_horizon` (365 days by default) and `deadline + accept_window` representable → 400 `deadline_beyond_horizon`, nothing stored | `fm_response_deadline_beyond_horizon_rejected`, `fm_far_future_deadline_cannot_kill_engine` |
| P3 | Maker quotes without collateral | Uncovered escrow at accept | `reserve()` at submit; short → 402, no quote stored | `fm_maker_insufficient_funds_at_quote_is_402` |
| P4 | Quote smaller than the leg | Partial fill | `size < notional` → 400; no partial fills exist | `fm_quote_too_small_rejected` |
| A6 | Quote would die inside the accept window | Requester accepts a quote whose maker is gone | Submit rejects `expires_at < response_deadline + accept_window` (400) and `expires_at ≤ now` (400) | `fm_quote_expiring_before_accept_window_rejected_at_submit` |
| P2 | Maker cancels a rival's quote | Competitor's collateral released | `quote.maker` must equal the caller → 403 | `fm_maker_cannot_cancel_others_quote` |
| P6 | Same quote cancelled twice | Double release | Only Live quotes cancel; second → 409 `quote_not_live` | `fm_cancel_released_quote_is_409` |

## Presented: the deadline match and the accept window

`tests/failure_modes/presented.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| M1 | Leg unmatched after others provisionally matched | Legs 1 and 3 reserved while leg 2 never fills | A match is a reservation not a lock; `Tick` fails the whole request and releases every quote; `lock_batch` never called | `fm_leg_unmatched_at_deadline_releases_all` |
| A7 | Late `Tick` pushes `accept_deadline` past a quote's expiry | Selecting a quote that expires mid-window | `select_best` requires `expires_at ≥ accept_deadline`; a worse but longer-lived quote wins | `fm_quote_expiring_before_accept_deadline_is_ineligible` |
| A8 | Quote already expired when `Tick` runs | Expired quote selected, or its collateral stuck | `Tick` releases Live quotes with `expires_at ≤ now` before matching | `fm_expired_quote_not_selected_and_released` |
| A11 | Identical prices on one leg | Non-deterministic winner | Ties break on engine `seq`, not `submitted_at` | `fm_tie_breaks_on_seq` |
| A13 | Worker so late that the contracts already resolved | Requester accepts knowing the outcome | `accept_deadline = min(now + accept_window, resolves_at)`; the window is already closed, accept → 409 and Failed(`accept_window_expired`), all released | `fm_accept_window_capped_at_resolution` |
| A4 | Maker cancels as the deadline `Tick` lands | Quote both released and selected | Serialised: cancel before `Tick` releases it and the next best is selected; after `Tick` it is Selected and cancel is 409 | `fm_cancel_and_tick_race` |
| A5 | Maker pulls a selected quote | Presented package with no collateral behind it | Cancel requires request Open; Presented → 409, reservation intact | `fm_selected_quote_cannot_be_cancelled` |
| P5 | Quote after the package is presented | Package changes under the requester | Request must be Open → 409, nothing reserved | `fm_quote_after_presented_is_409` |
| P1 | Stranger accepts or rejects | Locking someone else's funds | Ownership check before state check → 403 | `fm_non_owner_cannot_accept_or_reject` |
| A1 | Requester rejects the presented package | Selected and losing reservations held forever | Reject → Failed(`rejected`); every Live/Selected quote released | `fm_requester_reject_releases_selected_and_losers` |
| A2 | Requester never answers | Reservations held past the window | `Tick` with `now > accept_deadline` → Failed(`accept_window_expired`), all released; later accept 409 | `fm_accept_window_expiry_fails_request` |
| A12 | Accept arrives after `accept_deadline` with no `Tick` in between | Escrow created inside a closed window | Accept re-checks `accept_deadline` itself → Failed(`accept_window_expired`), every maker released, 409; at the deadline instant accept still succeeds | `fm_accept_past_deadline_without_tick_fails_request` |
| A3 | Accept and the expiry `Tick` arrive together | Escrow created *and* reservations released for one request | Actor serialises: first command wins, second sees Locked or Failed; accept re-checks `accept_deadline` itself | `fm_accept_and_tick_race` |

## Locked: the lock batch and what may follow it

`tests/failure_modes/locked.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| M2 | Requester cannot fund its side at accept | Maker collateral stranded, half-locked legs | `lock_batch` refuses before mutating; request → Failed(`insufficient_requester_funds`), all makers released, 402 | `fm_requester_insufficient_funds_at_accept` |
| M3 | One item of a lock batch is short | Earlier items escrowed, later ones not | Two-phase `lock_batch` under one mutex: validate all (summing per-party free), then apply; error touches no account | `fm_lock_batch_is_atomic` |
| A9 | Second accept on a Locked request | Double `lock_batch` | State must be Presented → 409; no ledger call | `fm_double_accept_is_409` |
| A10 | Accept, reject, or resolve after a terminal state | Second payout or refund | Settled/Unwound/Failed refuse everything with 409 | `fm_accept_after_terminal_is_409` |
| R3 | Resolve before escrow exists | Payout from nothing | Only Locked/Disputed resolve → 409 | `fm_resolve_before_locked_is_409` |
| R1 | Oracle says Invalid | 50/50 split or stuck escrow | `refund()` returns each side its own chunk → Unwound, terminal | `fm_resolve_invalid_unwinds_refunds_each_side` |

## Reported: the dispute window

`tests/failure_modes/reported.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| D1 | Oracle reports Yes/No | Payout before anyone can object | `Locked → Reported`: outcome and `dispute_deadline` recorded, no ledger call | `fm_report_does_not_pay_out` |
| D2 | Nobody files inside the window | Escrow held forever, or paid twice | First `Tick` with `now > dispute_deadline` settles the reported outcome once; filing afterwards is 409 | `fm_unfiled_report_settles_after_window` |
| D3 | Stranger or losing maker files | Anyone can freeze a settlement | Only the requester or a maker with a `Locked` quote may file → 403 otherwise | `fm_stranger_cannot_dispute` |
| D7 | Filing outside Reported, or a second report | Dispute on nothing; oracle silently changes its answer | Filing while Open, Presented, or Locked is 409; a resolve while Reported is 409 and the report stands | `fm_dispute_only_while_reported` |

## Disputed: adjudication or unwind

`tests/failure_modes/disputed.rs`

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| R2 | Oracle disputes, then decides | Payout during dispute | Disputed keeps escrow and counts as escrowed; later Yes/No pays out → Settled | `fm_disputed_then_yes_pays_out` |
| R4 | Every exit from Disputed | Payout or refund applied twice, or a dispute that quietly moves money | Repeated `disputed` is a 200 no-op; `no` pays each Yes-seller `n`; `invalid` refunds each poster its own chunk and is terminal | `fm_disputed_exits` |
| D4 | A party files | Money moves on a contested outcome | `Reported → Disputed`, `unwind_deadline` set, nothing moves; the old window no longer settles it; second filing 409 | `fm_party_dispute_holds_escrow` |
| D5 | Adjudication after a dispute | Reversal, or a second payout | From Disputed, `no` pays the Yes-sellers and `invalid` refunds each poster, immediately and once; further resolve or filing is 409 | `fm_adjudication_settles_or_unwinds_once` |
| D6 | No adjudication | Escrow stuck with no exit | First `Tick` with `now > unwind_deadline` refunds each poster once | `fm_unwind_timeout_refunds_each_poster` |

## Known gaps
- No last-look: quotes are firm at submit (collateral reserved immediately), so there is no post-selection cancel window to time — by design.
- No requester bond: a requester can reject or let the window lapse at no cost while maker collateral sat reserved — accepted for now.
- Self-quoting is allowed: a requester quoting its own request puts its own money on both sides, conservation holds, and every exit still applies; it can only crowd out other makers on that one request, which extracts nothing from them. Refusing it would be an identity rule, and identity is out of scope.
- No `resolution_timeout`: `Tick` ignores `Locked`, so escrow on a silent oracle is held indefinitely. `Disputed` does have a timer now (`unwind_timeout`).
- No dispute bond: filing is free, so a losing party can delay the winner by up to `unwind_timeout`, and if no adjudication arrives it is refunded rather than paid out. A bond forfeited to the winner when the report is upheld is the remedy, and a separate step.
- No idempotency key on quote submit: a retried POST reserves twice.
- A full command queue back-pressures handlers rather than returning 503.
