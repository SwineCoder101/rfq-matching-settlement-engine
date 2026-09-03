//! Why a command was refused. Mapped to HTTP in `crate::api`.

use crate::domain::{Amount, EmptyLegs, PartyId, RequestState};
use crate::ledger::InsufficientFunds;

/// Why a command was refused. Mapped to HTTP in `crate::api`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("not found")]
    NotFound,
    #[error("caller does not own this request or quote")]
    NotOwner,
    #[error("request is {actual:?}, expected {expected:?}")]
    WrongState {
        expected: RequestState,
        actual: RequestState,
    },
    #[error("quote is no longer live")]
    QuoteNotLive,
    #[error("quote has expired")]
    QuoteExpired,
    #[error("quote size is smaller than the leg notional")]
    QuoteTooSmall,
    #[error("quote expires before the accept window closes")]
    QuoteExpiresBeforeAcceptWindow,
    #[error("insufficient funds for {party}: needed {needed}, available {available}")]
    InsufficientFunds {
        party: PartyId,
        needed: Amount,
        available: Amount,
    },
    #[error("deadline is in the past")]
    DeadlineInPast,
    #[error("deadline is beyond the venue's response horizon")]
    DeadlineBeyondHorizon,
    #[error("a request must have at least one leg")]
    EmptyLegs,
    #[error("engine is not running")]
    Unavailable,
}

impl From<InsufficientFunds> for EngineError {
    fn from(e: InsufficientFunds) -> Self {
        EngineError::InsufficientFunds {
            party: e.party,
            needed: e.needed,
            available: e.available,
        }
    }
}

impl From<EmptyLegs> for EngineError {
    fn from(_: EmptyLegs) -> Self {
        EngineError::EmptyLegs
    }
}
