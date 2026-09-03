//! Failure modes and races, one module per request state. Every test is a row in
//! `docs/FAILURE_MODES.md`. Each asserts HTTP status, request and quote states, numeric
//! balances, and ends with `assert_conserved()`. Time is driven explicitly: the expiry worker
//! is not running, `advance_to` / `tick_at` deliver `Tick` by hand, so every interleaving is
//! reproducible.

#[path = "../common/mod.rs"]
mod common;
mod scenarios;

mod disputed;
mod locked;
mod open;
mod presented;
mod reported;
