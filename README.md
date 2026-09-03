# RFQ matching and settlement engine

A permissionless request-for-quote venue for binary contracts: requesters post legs, market makers post collateralized quotes, the engine presents the best package, escrows both sides atomically on accept, and pays out on a mocked oracle outcome.

## Run

```
cargo test                    # unit + integration tests (HTTP surface, races, conservation)
cargo run --example demo      # happy path and two failure paths with balances before/after
cargo run                     # serve on 127.0.0.1:3000 (override with RFQ_ADDR)
```

## Files

| File | Concern |
|---|---|
| `src/domain/` | value types (`ids`, `money`), enums (`state`), aggregates and invariants (`request`) |
| `src/matching.rs` | the one pure best-quote function |
| `src/engine.rs` | commands, the request state machine, and the actor that serializes it |
| `src/ledger.rs` | the `Ledger` port |
| `src/clock.rs` | the `Clock` port and the system clock |
| `src/mock/` | in-memory `MockLedger` and `MockClock` |
| `src/api.rs` | Axum router, `x-party-id` extractor, bodies, error mapping |
| `src/worker.rs` | periodic `Tick` to the engine |
| `tests/` | HTTP-level scenarios over a frozen clock; `common/` is the harness |

## Docs
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): domain, state machines, money flow, seconds-vs-days.
- [`docs/FAILURE_MODES.md`](docs/FAILURE_MODES.md): every failure or race, its mechanism, and the test that pins it.
- [`docs/RESOLUTION.md`](docs/RESOLUTION.md): how escrow unlocks; disputed, delayed, and invalid outcomes.
- [`ASSUMPTIONS.md`](ASSUMPTIONS.md): every judgment call, one line each.
