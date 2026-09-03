# Permissionless RFQ matching and settlement

A requester publishes a quote request (legs, deadline). Market makers answer with firm, collateralized quotes. At the response deadline the venue selects the best quote per leg and presents a package; the requester accepts or rejects it; on accept both sides lock into escrow in one batch; a mocked oracle outcome pays the winner.

Pricing is not this system's problem. Capital is: every intermediate state holds money and every participant is adversarial. Chain, payments, and the oracle are mocked. The venue is Tokio + Axum; handlers never move funds, they send commands to one engine actor.

## Domain objects

```mermaid
flowchart TB
    Request["RfqRequest<br/>aggregate root"]
    Leg["Leg<br/>contract, side, notional"]
    Quote["Quote<br/>price, size, expiry"]
    Escrow["Escrow<br/>p·n + (1−p)·n = n"]
    Ledger["LedgerAccount<br/>free / reserved / escrowed"]

    Request -->|"1 to N"| Leg
    Leg -->|"0 to N live quotes"| Quote
    Request -->|"0 or 1 package, after present"| Quote
    Request -->|"N escrows, only after accept"| Escrow
    Quote -->|"reserves maker collateral"| Ledger
    Escrow -->|"locks both sides"| Ledger

    classDef inflight fill:#dbeafe,stroke:#1d4ed8,color:#0f172a;
    classDef money fill:#fef3c7,stroke:#b45309,color:#0f172a;
    class Request,Leg inflight;
    class Quote,Escrow,Ledger money;
```

- **Leg**: one binary contract (an opaque id and a description that must be a complete resolution rule, see `ASSUMPTIONS.md`), the requester's side, a notional. Buying No at `1 - p` is selling Yes at `p`, so prices, collateral, and escrow are always in Yes terms: `buy_yes` / `sell_no` make the requester the Yes-buyer, `sell_yes` / `buy_no` make the maker the Yes-buyer.
- **Quote**: maker price, size, expiry. Reserves maker collateral while `Live` or `Selected`.
- **Escrow**: exists only after accept. Yes-buyer locked `p * n`, Yes-seller `(1 - p) * n`, total `n`.

Not a CLOB: no order book, no order types, no partial fills. Fill is atomic per request.

## Components

<img src="img/components.png" alt="System components" width="480">

The engine actor owns all requests and applies commands one at a time, so accept, cancel, and expiry cannot interleave. Matching is pure: eligible quotes are `Live`, `size >= notional`, unexpired, and `expires_at >= accept_deadline`; long-Yes legs take the lowest Yes price, short-Yes legs the highest; ties break on engine submit order.

### HTTP surface

Identity is the `x-party-id` header. Authorization: accept or reject only your own request, cancel only your own live quote. A request names a `tenor` preset; every leg resolves at `response_deadline + tenor`, and the accept window never extends past that instant.

- `POST /v1/ledger/credit` (mock faucet), `GET /v1/ledger/{party_id}`
- `POST /v1/requests`, `GET /v1/requests/{id}`
- `POST /v1/requests/{id}/quotes`, `DELETE /v1/quotes/{id}`
- `POST /v1/requests/{id}/accept`, `POST /v1/requests/{id}/reject`
- `POST /v1/oracle/resolve`

## Happy path

<img src="img/happy_path_states.png" alt="Happy path states" width="440">

The same path with the ledger calls at each step:

```mermaid
sequenceDiagram
    participant Req as Requester
    participant MM as MarketMaker
    participant Eng as EngineActor
    participant Led as Ledger
    participant Tick as ExpiryWorker

    rect rgb(219, 234, 254)
        Note over Req,Led: Open: requester stays free, makers reserve
        Req->>Eng: SubmitRequest
        Eng-->>Req: 201 Open
        MM->>Eng: SubmitQuote
        Eng->>Led: reserve MM collateral
        Eng-->>MM: 201 Live
    end
    rect rgb(254, 243, 199)
        Note over Eng,Tick: Response deadline: best quote per leg, no money moves
        Tick->>Eng: Tick past response_deadline
        Eng->>Eng: select_best per leg (Open to Presented)
    end
    rect rgb(220, 252, 231)
        Note over Req,Led: Accept: one atomic lock_batch, losers released
        Req->>Eng: Accept
        Eng->>Led: lock_batch all legs
        Eng->>Led: release losing quotes
        Eng-->>Req: 200 Locked
        Req->>Eng: Resolve Yes
        Eng->>Led: payout n per leg to the Yes-buyer
        Eng-->>Req: 200 Settled
    end
```

## Multi-leg abort: leg 2 of 3 unmatched

```mermaid
sequenceDiagram
    participant MM as MarketMakers
    participant Eng as EngineActor
    participant Led as Ledger
    participant Tick as ExpiryWorker

    rect rgb(219, 234, 254)
        MM->>Eng: quotes on leg1 and leg3
        Eng->>Led: reserve both
        Note over Eng: provisional match is a reservation, not a lock
    end
    rect rgb(254, 226, 226)
        Tick->>Eng: Tick at response_deadline
        Eng->>Eng: leg2 has no eligible quote
        Eng->>Led: release leg1 and leg3 (Open to Failed)
        Note over Led: lock_batch never called
    end
```

A provisional match is a reservation, not a lock. Escrow is request-atomic: there is no half-locked state.

## Money flow

Two buckets, never mixed: **reserved** is reversible and quote-scoped, posted at submit; **escrowed** is request-scoped, created only on accept, all legs in one `lock_batch`.

```mermaid
flowchart LR
    Free["Free"]
    Reserved["Reserved<br/>quote-scoped, reversible"]
    Escrowed["Escrowed<br/>request-scoped, one batch"]

    Free -->|"MM submit_quote"| Reserved
    Reserved -->|"cancel, lose, expire, Failed"| Free
    Reserved -->|"accept: MM side"| Escrowed
    Free -->|"accept: requester side"| Escrowed
    Escrowed -->|"Yes or No: payout to winner"| Free
    Escrowed -->|"Invalid: refund each poster"| Free

    classDef free fill:#dcfce7,stroke:#15803d,color:#0f172a;
    classDef held fill:#fef3c7,stroke:#b45309,color:#0f172a;
    classDef locked fill:#fecaca,stroke:#b91c1c,color:#0f172a;
    class Free free;
    class Reserved held;
    class Escrowed locked;
    linkStyle 0,2,3 stroke:#b45309,stroke-width:2px;
    linkStyle 1,5 stroke:#b91c1c,stroke-width:2px;
    linkStyle 4 stroke:#15803d,stroke-width:2px;
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

    classDef inflight fill:#dbeafe,stroke:#1d4ed8,color:#0f172a
    classDef held fill:#fef3c7,stroke:#b45309,color:#0f172a
    classDef done fill:#dcfce7,stroke:#15803d,color:#0f172a
    classDef undone fill:#fee2e2,stroke:#b91c1c,color:#0f172a
    class Open,Presented inflight
    class Locked,Disputed held
    class Settled done
    class Failed,Unwound undone
```

`Settled`, `Unwound`, and `Failed` are terminal: any further accept, reject, or resolve is `409`. `Locked` and `Disputed` have no timer today; see `docs/RESOLUTION.md`.

### Quote lifecycle

<img src="img/quote_lifecycle_maker_view.png" alt="Quote lifecycle, market maker view" width="560">

`Live` leaves for `Released` on cancel, own expiry while the request is `Open`, losing at accept, or the request failing. `Selected` leaves for `Released` on reject, accept-window expiry, or a refused `lock_batch`. A quote's own expiry is not honoured once the request is `Presented`.

## Quote lifetime: seconds vs days

Invariant to that choice: the state machines and who may trigger each transition, reservation versus escrow, best-quote comparison, binary payoff math, and request-atomic `lock_batch`.

Not invariant: how `Tick` is scheduled (an in-process interval today; a durable job for day-long windows), whether quotes are firm at submit or indicative-then-firm (reserving collateral for days is expensive, so a `ConfirmQuote` step before `Presented` would appear), and clock-skew tolerance.

Deadlines are absolute timestamps and `Tick` carries `now`, so seconds-to-days is data and worker period, not a rewrite of `Locked` or `Settled`.
