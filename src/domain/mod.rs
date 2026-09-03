//! Domain model: value types, aggregates, and their invariants. No I/O.
//!
//! Types with an invariant (`Price`, `ContractId`, `ContractDescription`, `Leg`) have no
//! `Deserialize`: the API parses raw input and calls the checked constructor. Plain enums
//! and aggregates derive `Serialize` because the API returns them verbatim.

pub mod ids;
pub mod money;
pub mod request;
pub mod state;

pub use ids::{
    ContractDescription, ContractId, InvalidContractDescription, InvalidContractId, LegId, PartyId,
    QuoteId, RequestId, Seq,
};
pub use money::{Amount, InvalidPrice, Price};
pub use request::{EmptyLegs, Escrow, Leg, Package, Quote, RfqRequest, Selection, ZeroNotional};
pub use state::{FailReason, LegSide, OracleOutcome, QuoteState, RequestState};
