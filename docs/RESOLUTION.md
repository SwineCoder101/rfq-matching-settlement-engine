# Resolution

How escrow unlocks once a request is `Locked`, and what the engine does when the oracle is disputed, late, or the contract wording turns out to be ambiguous. Companion to `docs/ARCHITECTURE.md` (state machine, money flow) and `docs/FAILURE_MODES.md` (rows R1–R3).

## What is locked

Resolution only ever touches escrow. Reservations are gone by the time a request is `Locked`: accept converted every selected quote's reservation into escrow in one `lock_batch` and released every losing quote.

Each leg holds two escrow handles in the ledger, one per poster:

| Handle | Posted by | Amount |
|---|---|---|
| `yes_buyer` | whoever is long Yes on the leg | `p * n` (floored) |
| `yes_seller` | whoever is short Yes on the leg | `n - p * n` |

Which party is the Yes-buyer follows the leg side: `buy_yes` / `sell_no` make the requester the Yes-buyer, `sell_yes` / `buy_no` make the maker the Yes-buyer. The two amounts always sum to `n`, so the pool for a leg is exactly the notional. The engine keeps a `(request_id, leg_id) → (yes_buyer handle, yes_seller handle)` map; the ledger keeps `handle → (poster, amount)`.

A request with several legs holds several independent pools. One `Resolve` command acts on all of them at once.

## Unlock mechanics

`POST /v1/oracle/resolve { request_id, outcome }` becomes `Command::Resolve` and runs on the engine actor, serialised with everything else. The request must be `Locked` or `Disputed`; anything else is `409 wrong_state` and no ledger call happens (R3).

| Outcome | Ledger calls per leg | Request state | Terminal |
|---|---|---|---|
| `yes` | `payout(yes_buyer_handle, yes_buyer)`, `payout(yes_seller_handle, yes_buyer)` | `Settled` | yes |
| `no` | `payout(yes_buyer_handle, yes_seller)`, `payout(yes_seller_handle, yes_seller)` | `Settled` | yes |
| `invalid` | `refund(yes_buyer_handle)`, `refund(yes_seller_handle)` | `Unwound` | yes |
| `disputed` | none | `Disputed` | no |

`payout` moves the chunk from the poster's `escrowed` bucket to the winner's `free` bucket. The winner receives both chunks, so a Yes-buyer who put up `p * n` walks away with `n`. `refund` moves each chunk back to its own poster's `free`. In both cases the handle is consumed and removed from the engine's escrow map; a repeat call on the same handle is a no-op in the ledger, and a repeat `Resolve` on the request is a `409` because the state is now terminal (A10).

Every leg of a request resolves with the same outcome. There is no per-leg resolution today; the parlay spec in `tests/parlay.rs` is where that changes.

## Disputed

`disputed` is a hold, not a decision. The engine flips the request to `Disputed` and does nothing to the ledger: both chunks stay in `escrowed`, both parties still see the money in their balance under that bucket, and neither can withdraw it. Conservation holds because nothing moved.

The only exits from `Disputed` are a later `Resolve`. `yes` or `no` pays out exactly as from `Locked` (R2). `invalid` unwinds. A second `disputed` leaves the state unchanged and returns `200`.

## Delayed

"Delayed" means nobody has called resolve. The engine does not poll an oracle: the `Oracle` trait and `MockOracle` exist in `src/domain/ports.rs` and `src/mocks/oracle.rs`, but the engine is constructed without one and resolution is push-only over HTTP. A contract the operator has not resolved is indistinguishable from one that will never be resolved.

Mechanically the request sits in `Locked`. The expiry worker's `Tick` skips `Locked` and `Disputed` entirely, so no timeout fires and escrow is held indefinitely. Nothing leaks in the accounting sense, since every unit is still in someone's `escrowed` bucket, but capital is stuck until a resolve arrives.

The architecture doc specifies the intended policy, and it is **not implemented**:

- After `resolution_timeout` with no outcome: `Locked → Disputed`. Still no payout; the state change is a signal, not a transfer.
- After `unwind_timeout` in `Disputed`: `Disputed → Unwound`, refund both chunks of every leg exactly as `invalid` does.

Both would be `EngineConfig` fields alongside `accept_window`, and both would be applied inside `Engine::tick` using the carried `now` against a timestamp recorded at lock time. `RfqRequest` does not yet record when it became `Locked` or `Disputed`, so that field is the first prerequisite.

## Ambiguous wording

The engine never reads contract text. A leg carries a `ContractId` and a free-text `ContractDescription`, both opaque; nothing in the domain compares, parses, or interprets them. Whether the wording is ambiguous is entirely the oracle operator's judgement, and the operator expresses it with `invalid`.

`invalid` unwinds immediately: each poster gets its own chunk back, the request is `Unwound`, terminal. Two things it deliberately is not:

- **Not a 50/50 split.** A Yes-buyer who posted `p * n` gets `p * n` back, not `n / 2`. Splitting the pool evenly would transfer money between parties on a contract that was never valid.
- **Not a dispute.** Ambiguity is a final answer about the contract, so it ends the request. `disputed` is for a contested but resolvable outcome and keeps the request alive.

If wording ambiguity surfaces while the request is `Disputed`, `invalid` works the same way from there.

## Who may resolve

Today: anyone. The resolve handler does not use the `x-party-id` extractor, so any caller who knows a request id can settle it in either direction. This is row X2 in `docs/FAILURE_MODES.md` and its test is red on purpose. The intended fix is an oracle party id in `EngineConfig` checked before the state check, returning `403 not_owner` on mismatch, matching how accept and reject check ownership.

## Invariants

- Escrow is created only by accept and destroyed only by `payout` or `refund`. No other path touches the `escrowed` bucket.
- For each leg, `yes_buyer_amount + yes_seller_amount == notional`, and the winner of a `yes` / `no` receives exactly `notional`.
- A request is resolved at most once: after `Settled` or `Unwound` every command is `409`.
- `Disputed` never moves money.
- Venue-wide, the sum of `escrowed` across parties equals the sum of notionals on `Locked` and `Disputed` requests.
