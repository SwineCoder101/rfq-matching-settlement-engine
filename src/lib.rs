//! Permissionless RFQ matching and settlement engine. See `docs/ARCHITECTURE.md`.
//!
//! - [`domain`]: value types, aggregates, and their invariants. No I/O.
//! - [`matching`]: the one pure best-quote function.
//! - [`engine`]: commands, the request state machine, and the actor that serializes it.
//! - [`ledger`] / [`clock`]: the ports the engine talks to, with in-memory implementations.
//! - [`api`]: Axum router, identity extractor, bodies, and error mapping.
//! - [`worker`]: the expiry worker that ticks the engine.
#![forbid(unsafe_code)]

pub mod api;
pub mod clock;
pub mod domain;
pub mod engine;
pub mod ledger;
pub mod matching;
pub mod worker;
