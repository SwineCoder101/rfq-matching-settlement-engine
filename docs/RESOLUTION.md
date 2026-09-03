# Resolution

How escrow unlocks once a request is `Locked`, and what happens when the oracle is disputed, late, or the contract wording turns out to be ambiguous. Rows R1–R3 in `docs/FAILURE_MODES.md` pin this.

## What is locked

By `Locked`, reservations are gone: accept converted every selected quote's reservation into escrow in one `lock_batch` and released every loser. Each leg holds two ledger handles, one per poster: the Yes-buyer's `p * n` (floored) and the Yes-seller's `n - p * n`. They sum to exactly `n`, so a leg's pool is its notional. A multi-leg request holds one pool per leg; one `Resolve` acts on all of them with the same outcome.

## Unlock mechanics

`POST /v1/oracle/resolve { request_id, outcome }` runs on the engine actor, serialized with every other command. The request must be `Locked` or `Disputed`; anything else is `409` with no ledger call.

| Outcome | Per leg | State | Terminal |
|---|---|---|---|
| `yes` | pay both chunks to the Yes-buyer | `Settled` | yes |
| `no` | pay both chunks to the Yes-seller | `Settled` | yes |
| `invalid` | refund each chunk to its own poster | `Unwound` | yes |
| `disputed` | nothing | `Disputed` | no |

`payout` moves a chunk from the poster's `escrowed` to the winner's `free`, so a Yes-buyer who put up `p * n` walks away with `n`. `refund` moves each chunk back to its poster's `free`. Handles are consumed either way; a second `Resolve` is `409` because the state is terminal.

## Disputed

A hold, not a decision. The state flips to `Disputed` and the ledger is untouched: both chunks stay escrowed, neither party can withdraw. The only exits are a later `yes` / `no` (pays out exactly as from `Locked`) or `invalid` (unwinds). A repeated `disputed` is a `200` no-op.

## Delayed

"Delayed" means nobody has called resolve. Resolution is push-only: the engine does not poll anything, so a contract nobody has resolved is indistinguishable from one that never will be. The request sits in `Locked`; `Tick` skips `Locked` and `Disputed`, so escrow is held indefinitely. Nothing leaks in the accounting sense, but capital is stuck until an outcome arrives.

The intended policy, **not implemented**:

- After `resolution_timeout` with no outcome: `Locked → Disputed`. A signal, not a transfer.
- After `unwind_timeout` in `Disputed`: `Disputed → Unwound`, refund both chunks of every leg exactly as `invalid` does.

Both would be `EngineConfig` fields next to `accept_window`, applied inside `Engine::tick` against a timestamp recorded when the request became `Locked`. `RfqRequest` does not record that instant yet; adding it is the first step.

## Ambiguous wording

The engine never reads contract text: `ContractId` and `ContractDescription` are opaque. Whether wording is ambiguous is the oracle operator's judgment, expressed as `invalid`, which unwinds immediately. Deliberately it is not a 50/50 split (that would transfer money on a contract that was never valid) and not a dispute (ambiguity is final, so it ends the request). `invalid` from `Disputed` behaves the same.

## Invariants

- Escrow is created only by accept and destroyed only by `payout` or `refund`.
- Per leg, `yes_buyer_amount + yes_seller_amount == notional`; a `yes` / `no` winner receives exactly `notional`.
- A request resolves at most once; `Disputed` never moves money.
- Venue-wide, escrowed equals the notionals of `Locked` and `Disputed` requests.
