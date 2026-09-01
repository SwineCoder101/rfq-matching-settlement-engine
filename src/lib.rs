//! Permissionless RFQ matching and settlement engine.
//!
//! Layout follows `docs/ARCHITECTURE.md`:
//! - [`domain`] — aggregates, value types, engine commands, ports (traits), and the pure
//!   best-quote matching function. No I/O, no serde `Deserialize`.
//! - [`engine`] — the state machine and the Tokio actor that serializes every mutation.
//! - [`worker`] — the expiry worker that ticks the engine.
//! - [`mocks`] — in-memory `Ledger`, `Oracle`, and `Clock` implementations.
//! - [`api`] — HTTP DTOs, error mapping, `x-party-id` extractor, and the Axum router.
#![forbid(unsafe_code)]

pub mod api;
pub mod domain;
pub mod engine;
pub mod mocks;
pub mod worker;
