# Assumptions

Judgment calls the brief left open, one per line, as *assumption → consequence*. Each is what the code does today.

- Identity is the `x-party-id` header, trusted as given → no authentication; the only authorization is ownership (accept/reject your own request, cancel your own quote).
- `POST /v1/oracle/resolve` is ungated → any caller can settle a `Locked` request; the oracle is a trusted mocked input, gating it is out of scope.
- Contract descriptions are complete resolution rules written by the requester (measurable condition, data source, UTC instant, `No` branch) → the venue stores them verbatim, checks only non-blank and ≤ 1000 characters, and never interprets them; unclear text is the requester's cost, not a resolution case, and `invalid` means unresolvable as written.
- One outcome resolves every leg of a request → legs on different contracts in one request settle together; per-leg resolution does not exist.
- Yes and No pay the winner the full notional; Invalid refunds each poster its own chunk; Disputed holds → no 50/50 split, no partial payout, no interest.
- `Locked` and `Disputed` have no timer → escrow is held until an outcome arrives; the timeout policy is designed in `docs/RESOLUTION.md` but not built.
- The requester posts nothing at Open → it can open, let makers reserve, and reject for free (griefing accepted); a requester bond is future work.
- Maker collateral at submit is its escrow side at the quote price for the full leg notional → quotes are firm and fully collateralized; `size` above notional reserves nothing extra.
- Quotes are firm from submit: cancel only while the request is `Open` and the quote `Live` → no last look; a `Selected` quote cannot be pulled.
- Losing quotes stay reserved through `Presented` and are released at accept, reject, or window expiry → a maker cannot spend the same collateral into a rival RFQ while a package it lost is still live.
- Fill is atomic per request: a quote must cover the whole leg (`size >= notional`) and every leg must match → no partial fills; one unmatched leg fails the request and releases everything.
- Money is `u64` minor units in one currency; Yes-buyer lock is floored, Yes-seller takes the remainder → escrow sums to the notional exactly; overflow panics rather than wraps.
- Prices are Yes prices in basis points, `1..=9_999` → `buy_no` at `p` is `sell_yes` at `1 - p`; 0% and 100% are not trades.
- Deadlines are client-supplied absolute UTC timestamps, checked only against the venue clock → clock skew is the client's problem; there is no maximum horizon.
- `Tick` carries the worker's `now`; the accept window starts at the tick that presents, not at the response deadline → a late worker extends the window, so matching requires quotes to outlive `now + accept_window`.
- Boundaries: accept is allowed at exactly `accept_deadline`; a quote whose `expires_at == now` is expired; the response deadline instant itself presents.
- Ties on price break on engine-assigned submit order, never on timestamps → deterministic winner regardless of clock resolution.
- The engine is one actor over a bounded queue → mutations are serial so accept/expiry/cancel cannot race; throughput is bounded by design and a full queue back-pressures handlers instead of returning 503.
- Ledger, oracle, and clock are in-memory; nothing persists → a restart loses every request and balance.
- A requester may quote its own request → known gap X1 in `docs/FAILURE_MODES.md`, test ignored.
