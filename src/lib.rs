//! Permissionless RFQ matching and settlement engine.
//!
//! Layout follows `docs/ARCHITECTURE.md`:
//! - [`domain`] — aggregates, value types, engine commands, ports (traits), and the pure
//!   best-quote matching function. No I/O, no serde `Deserialize`.
//! - [`mocks`] — in-memory `Ledger`, `Oracle`, and `Clock` implementations.
//! - [`api`] — HTTP request/response DTOs (serde) and the `EngineError` → HTTP mapping.
//!
//! The engine actor, HTTP handlers, and expiry worker are intentionally absent for now.
#![forbid(unsafe_code)]

pub mod api;
pub mod domain;
pub mod mocks;
