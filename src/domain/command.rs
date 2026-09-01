//! Engine actor commands and the error type every reply carries.

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

use super::ids::{LegId, PartyId, QuoteId, RequestId};
use super::money::{Amount, Price};
use super::ports::InsufficientFunds;
use super::request::Leg;
use super::state::{OracleOutcome, RequestState};

/// One-shot reply channel from the engine actor back to the caller.
pub type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

/// Everything the engine actor can be asked to do. Handlers and the expiry worker send these;
/// the actor applies them one at a time so accept and expiry cannot race.
#[derive(Debug)]
pub enum Command {
    /// Requester opens an RFQ. Replies with the new request id.
    SubmitRequest {
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
        reply: Reply<RequestId>,
    },
    /// Market maker quotes a leg. Reserves collateral. Replies with the new quote id.
    SubmitQuote {
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
        reply: Reply<QuoteId>,
    },
    /// Market maker cancels their own live quote while the request is still `Open`.
    CancelQuote {
        maker: PartyId,
        quote_id: QuoteId,
        reply: Reply<()>,
    },
    /// Requester accepts the presented package. Replies with the resulting state (`Locked`).
    Accept {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RequestState>,
    },
    /// Requester rejects the presented package. Replies with the resulting state (`Failed`).
    Reject {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RequestState>,
    },
    /// Oracle operator reports an outcome. Replies with the resulting state
    /// (`Settled`, `Disputed`, or `Unwound`).
    Resolve {
        request_id: RequestId,
        outcome: OracleOutcome,
        reply: Reply<RequestState>,
    },
    /// Expiry worker heartbeat. Deadlines are absolute; `now` is carried, never read.
    Tick { now: DateTime<Utc> },
}

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
    #[error("quote has expired")]
    QuoteExpired,
    #[error("quote size is smaller than the leg notional")]
    QuoteTooSmall,
    #[error("quote expires before the accept window closes")]
    QuoteExpiresBeforeAcceptWindow,
    #[error("price must be within 1..=9999 basis points")]
    InvalidPrice,
    #[error("insufficient funds for {party}: needed {needed}, available {available}")]
    InsufficientFunds {
        party: PartyId,
        needed: Amount,
        available: Amount,
    },
    #[error("deadline is in the past")]
    DeadlineInPast,
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

impl From<super::money::InvalidPrice> for EngineError {
    fn from(_: super::money::InvalidPrice) -> Self {
        EngineError::InvalidPrice
    }
}
