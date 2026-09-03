//! In-memory implementations of the ports, used by tests, the demo, and `main` (the venue has
//! no real ledger or chain in this exercise). The oracle is not a port: resolution is pushed
//! over HTTP by whoever calls `POST /v1/oracle/resolve`.

pub mod clock;
pub mod ledger;

pub use clock::MockClock;
pub use ledger::{MockLedger, PartyAudit};
