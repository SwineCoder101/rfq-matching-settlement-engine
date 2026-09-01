//! Domain model. Everything here is I/O-free and free of `serde::Deserialize`;
//! parsing from the wire happens in `crate::api`.

pub mod command;
pub mod ids;
pub mod matching;
pub mod money;
pub mod ports;
pub mod request;
pub mod state;

pub use command::{Command, EngineError, Reply};
pub use ids::{ContractId, InvalidContractId, LegId, PartyId, QuoteId, RequestId, Seq};
pub use matching::select_best;
pub use money::{Amount, InvalidPrice, Price};
pub use ports::{
    Clock, EscrowHandle, InsufficientFunds, Ledger, LockBatchError, LockItem, Oracle,
    ReservationId,
};
pub use request::{
    EmptyLegs, Escrow, LedgerAccount, Leg, Package, Quote, RfqRequest, Selection, ZeroNotional,
};
pub use state::{FailReason, LegSide, OracleOutcome, QuoteState, RequestState};
