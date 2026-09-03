# Permissionless RFQ matching and settlement

A requester publishes a quote request (legs, deadline). Market makers answer with firm, collateralized quotes. At the response deadline the venue selects the best quote per leg and presents a package; the requester accepts or rejects it; on accept both sides lock into escrow in one batch; a mocked oracle outcome pays the winner.

Pricing is not this system's problem. Capital is: every intermediate state holds money and every participant is adversarial. Chain, payments, and the oracle are mocked. The venue is Tokio + Axum; handlers never move funds, they send commands to one engine actor.

## Domain objects

```mermaid
flowchart TB
    Request["RfqRequest"]
    Leg["Leg"]
    Quote["Quote"]
    Escrow["Escrow"]
    Ledger["LedgerAccount"]

    Request -->|"1 to N"| Leg
    Leg -->|"0 to N live quotes"| Quote
    Request -->|"0 or 1 package after present"| Quote
    Request -->|"N escrows only after accept"| Escrow
    Quote -->|"reserves"| Ledger
    Escrow -->|"locks"| Ledger
```

- **Leg**: one binary contract (opaque id and description), the requester's side, a notional. Buying No at `1 - p` is selling Yes at `p`, so prices, collateral, and escrow are always in Yes terms: `buy_yes` / `sell_no` make the requester the Yes-buyer, `sell_yes` / `buy_no` make the maker the Yes-buyer.
- **Quote**: maker price, size, expiry. Reserves maker collateral while `Live` or `Selected`.
- **Escrow**: exists only after accept. Yes-buyer locked `p * n`, Yes-seller `(1 - p) * n`, total `n`.

Not a CLOB: no order book, no order types, no partial fills. Fill is atomic per request.

## Components

```mermaid
flowchart TB
    subgraph clients [Clients]
        Requester
        MarketMaker
        Oracle
    end
    subgraph http [Axum]
        Router --> PartyExtractor["x-party-id extractor"] --> Handlers
    end
    subgraph runtime [Tokio]
        ExpiryWorker["Expiry worker"]
        EngineActor["Engine actor"]
    end
    Requester --> Router
    MarketMaker --> Router
    Oracle --> Router
    Handlers -->|"command plus oneshot"| EngineActor
    ExpiryWorker -->|"Tick now"| EngineActor
    EngineActor --> Matching["select_best (pure)"]
    EngineActor --> Ledger["Ledger (in-memory)"]
    EngineActor --> Clock
```

The engine actor owns all requests and applies commands one at a time, so accept, cancel, and expiry cannot interleave. Matching is pure: eligible quotes are `Live`, `size >= notional`, unexpired, and `expires_at >= accept_deadline`; long-Yes legs take the lowest Yes price, short-Yes legs the highest; ties break on engine submit order.

### HTTP surface

Identity is the `x-party-id` header. Authorization: accept or reject only your own request, cancel only your own live quote.

- `POST /v1/ledger/credit` (mock faucet), `GET /v1/ledger/{party_id}`
- `POST /v1/requests`, `GET /v1/requests/{id}`
- `POST /v1/requests/{id}/quotes`, `DELETE /v1/quotes/{id}`
- `POST /v1/requests/{id}/accept`, `POST /v1/requests/{id}/reject`
- `POST /v1/oracle/resolve`

## Happy path

```mermaid
sequenceDiagram
    participant Req as Requester
    participant MM as MarketMaker
    participant Eng as EngineActor
    participant Led as Ledger
    participant Tick as ExpiryWorker

    Req->>Eng: SubmitRequest (Open)
    MM->>Eng: SubmitQuote
    Eng->>Led: reserve MM collateral
    Tick->>Eng: Tick past response_deadline
    Eng->>Eng: select best per leg (Open to Presented)
    Req->>Eng: Accept
    Eng->>Led: lock_batch all legs, release losers (Presented to Locked)
    Req->>Eng: Resolve Yes
    Eng->>Led: payout n per leg to Yes-buyer (Locked to Settled)
```

## Multi-leg abort: leg 2 of 3 unmatched

```mermaid
sequenceDiagram
    participant MM as MarketMakers
    participant Eng as EngineActor
    participant Led as Ledger
    participant Tick as ExpiryWorker

    MM->>Eng: quotes on leg1 and leg3
    Eng->>Led: reserve both
    Tick->>Eng: Tick at response_deadline
    Eng->>Eng: leg2 has no eligible quote
    Eng->>Led: release leg1 and leg3 (Open to Failed)
    Note over Led: lock_batch never called
```

A provisional match is a reservation, not a lock. Escrow is request-atomic: there is no half-locked state.

## Money flow

Two buckets, never mixed: **reserved** is reversible and quote-scoped, posted at submit; **escrowed** is request-scoped, created only on accept, all legs in one `lock_batch`.

```mermaid
flowchart LR
    Free -->|"MM submit_quote"| Reserved
    Reserved -->|"cancel, lose, expire, or Failed"| Free
    Reserved -->|"accept: MM side"| Escrowed
    Free -->|"accept: requester side"| Escrowed
    Escrowed -->|"Yes or No: payout to winner"| Free
    Escrowed -->|"Invalid: refund each poster"| Free
```

- **Open**: requester stays free (price unknown). Every live quote has maker collateral reserved.
- **Presented**: selected quotes are firm (no cancel). Unselected quotes stay reserved until accept, reject, or window expiry so the same collateral cannot be spent into another RFQ meanwhile.
- **Locked**: selected reservations plus the requester's free balance move to escrow in one batch, or nothing moves. Losers are released.
- **Settled**: escrow pays `n` per leg to the winner. **Unwound / Failed**: every hold returns to its poster.

Conservation: per party, `free + reserved + escrowed` equals credits minus paid out plus received. Venue-wide, escrowed equals the notionals of `Locked` and `Disputed` requests. The tests assert both after every scenario.

## Request state machine

```mermaid
stateDiagram-v2
    [*] --> Open: SubmitRequest
    Open --> Open: SubmitQuote or CancelQuote
    Open --> Presented: Tick, every leg has an eligible best quote
    Open --> Failed: Tick, any leg unmatched
    Presented --> Locked: Accept, lock_batch succeeds
    Presented --> Failed: Reject, accept window expiry, or requester cannot fund lock_batch
    Locked --> Settled: Resolve Yes or No
    Locked --> Disputed: Resolve Disputed
    Locked --> Unwound: Resolve Invalid
    Disputed --> Settled: Resolve Yes or No
    Disputed --> Unwound: Resolve Invalid
    Failed --> [*]
    Settled --> [*]
    Unwound --> [*]
```

`Settled`, `Unwound`, and `Failed` are terminal: any further accept, reject, or resolve is `409`. `Locked` and `Disputed` have no timer today; see `docs/RESOLUTION.md`.

### Quote lifecycle

```mermaid
stateDiagram-v2
    [*] --> Live: SubmitQuote reserves collateral
    Live --> Released: cancel, expiry, lose at accept, or request Failed
    Live --> Selected: request Presented
    Selected --> Locked: Accept
    Selected --> Released: Reject, window expiry, or lock_batch refused
```

## Quote lifetime: seconds vs days

Invariant to that choice: the state machines and who may trigger each transition, reservation versus escrow, best-quote comparison, binary payoff math, and request-atomic `lock_batch`.

Not invariant: how `Tick` is scheduled (an in-process interval today; a durable job for day-long windows), whether quotes are firm at submit or indicative-then-firm (reserving collateral for days is expensive, so a `ConfirmQuote` step before `Presented` would appear), and clock-skew tolerance.

Deadlines are absolute timestamps and `Tick` carries `now`, so seconds-to-days is data and worker period, not a rewrite of `Locked` or `Settled`.
