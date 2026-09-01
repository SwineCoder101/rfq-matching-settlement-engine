//! The engine: owns every `RfqRequest`, applies commands one at a time, and is the only thing
//! that touches the ledger. [`actor::spawn_engine`] runs it on a Tokio task behind an mpsc.

mod actor;
mod core;

pub use actor::{EngineHandle, spawn_engine};
pub use core::{Engine, EngineConfig, SharedClock, SharedLedger};
