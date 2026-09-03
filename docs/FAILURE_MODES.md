# Failure modes

The engine actor serialises every mutation; the ledger is reserve (quote-scoped) then escrow (request-scoped, one `lock_batch` at accept). Every row is pinned by the named test in `tests/failure_modes.rs`, and every test there is a row here. Each test asserts HTTP status, request and quote states, balances, and ledger conservation.

| # | Failure or race | What could leak | Mechanism | Test |
|---|-----------------|-----------------|-----------|------|
| H1 | Baseline: three legs quoted, accepted, resolved Yes | — | Reserve at quote, one `lock_batch` at accept, `payout` of `n` per leg to the Yes-buyer | `fm_happy_path_three_legs` |
| M1 | Leg unmatched after others provisionally matched | Legs 1 and 3 reserved while leg 2 never fills | A match is a reservation not a lock; `Tick` fails the whole request and releases every quote; `lock_batch` never called | `fm_leg_unmatched_at_deadline_releases_all` |
| M2 | Requester cannot fund its side at accept | Maker collateral stranded, half-locked legs | `lock_batch` refuses before mutating; request → Failed(`insufficient_requester_funds`), all makers released, 402 | `fm_requester_insufficient_funds_at_accept` |
| M3 | One item of a lock batch is short | Earlier items escrowed, later ones not | Two-phase `lock_batch` under one mutex: validate all (summing per-party free), then apply; error touches no account | `fm_lock_batch_is_atomic` |
| A1 | Requester rejects the presented package | Selected and losing reservations held forever | Reject → Failed(`rejected`); every Live/Selected quote released | `fm_requester_reject_releases_selected_and_losers` |
| A2 | Requester never answers | Reservations held past the window | `Tick` with `now > accept_deadline` → Failed(`accept_window_expired`), all released; later accept 409 | `fm_accept_window_expiry_fails_request` |
| A3 | Accept and the expiry `Tick` arrive together | Escrow created *and* reservations released for one request | Actor serialises: first command wins, second sees Locked or Failed; accept re-checks `accept_deadline` itself | `fm_accept_and_tick_race` |
| A4 | Maker cancels as the deadline `Tick` lands | Quote both released and selected | Serialised: cancel before `Tick` releases it and the next best is selected; after `Tick` it is Selected and cancel is 409 | `fm_cancel_and_tick_race` |
| A5 | Maker pulls a selected quote | Presented package with no collateral behind it | Cancel requires request Open; Presented → 409, reservation intact | `fm_selected_quote_cannot_be_cancelled` |
| A6 | Quote would die inside the accept window | Requester accepts a quote whose maker is gone | Submit rejects `expires_at < response_deadline + accept_window` (400) and `expires_at ≤ now` (400) | `fm_quote_expiring_before_accept_window_rejected_at_submit` |
| A7 | Late `Tick` pushes `accept_deadline` past a quote's expiry | Selecting a quote that expires mid-window | `select_best` requires `expires_at ≥ accept_deadline`; a worse but longer-lived quote wins | `fm_quote_expiring_before_accept_deadline_is_ineligible` |
| A8 | Quote already expired when `Tick` runs | Expired quote selected, or its collateral stuck | `Tick` releases Live quotes with `expires_at ≤ now` before matching | `fm_expired_quote_not_selected_and_released` |
| A9 | Second accept on a Locked request | Double `lock_batch` | State must be Presented → 409; no ledger call | `fm_double_accept_is_409` |
| A10 | Accept, reject, or resolve after a terminal state | Second payout or refund | Settled/Unwound/Failed refuse everything with 409 | `fm_accept_after_terminal_is_409` |
| A11 | Identical prices on one leg | Non-deterministic winner | Ties break on engine `seq`, not `submitted_at` | `fm_tie_breaks_on_seq` |
| P1 | Stranger accepts or rejects | Locking someone else's funds | Ownership check before state check → 403 | `fm_non_owner_cannot_accept_or_reject` |
| P2 | Maker cancels a rival's quote | Competitor's collateral released | `quote.maker` must equal the caller → 403 | `fm_maker_cannot_cancel_others_quote` |
| P3 | Maker quotes without collateral | Uncovered escrow at accept | `reserve()` at submit; short → 402, no quote stored | `fm_maker_insufficient_funds_at_quote_is_402` |
| P4 | Quote smaller than the leg | Partial fill | `size < notional` → 400; no partial fills exist | `fm_quote_too_small_rejected` |
| P5 | Quote after the package is presented | Package changes under the requester | Request must be Open → 409, nothing reserved | `fm_quote_after_presented_is_409` |
| P6 | Same quote cancelled twice | Double release | Only Live quotes cancel; second → 409 `quote_not_live` | `fm_cancel_released_quote_is_409` |
| P7 | No identity header | Anonymous mutation | `x-party-id` extractor → 401 before any handler runs | `fm_missing_party_header_is_401` |
| P8 | Unknown request, leg, or quote id | — | 404, no side effects | `fm_unknown_ids_are_404` |
| P9 | Price 0 or 100 %, zero notional, empty legs, blank contract | Untradeable leg | Parsed into newtypes at the boundary → 400 | `fm_invalid_body_rejected` |
| P10 | Response deadline in the past | Request that can never present | `response_deadline ≤ now` → 400 | `fm_response_deadline_in_past_rejected` |
| R1 | Oracle says Invalid | 50/50 split or stuck escrow | `refund()` returns each side its own chunk → Unwound, terminal | `fm_resolve_invalid_unwinds_refunds_each_side` |
| R2 | Oracle disputes, then decides | Payout during dispute | Disputed keeps escrow and counts as escrowed; later Yes/No pays out → Settled | `fm_disputed_then_yes_pays_out` |
| R3 | Resolve before escrow exists | Payout from nothing | Only Locked/Disputed resolve → 409 | `fm_resolve_before_locked_is_409` |
| X1 | Requester quotes its own request | Requester sets its own price against itself | none yet — engine accepts it (test red) | `fm_self_quote_rejected` |

## Known gaps
- No last-look: quotes are firm at submit (collateral reserved immediately), so there is no post-selection cancel window to time — by design.
- No requester bond: a requester can reject or let the window lapse at no cost while maker collateral sat reserved — accepted for now.
- No `resolution_timeout` / `unwind_timeout`: `Tick` ignores Locked and Disputed, so escrow on a silent oracle is held indefinitely (design in `RESOLUTION.md`).
- No idempotency key on quote submit: a retried POST reserves twice.
- A full command queue back-pressures handlers rather than returning 503.
