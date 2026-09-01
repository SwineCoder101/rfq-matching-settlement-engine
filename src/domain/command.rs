//! Engine actor commands and the error type every reply carries.

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

use super::ids::{LegId, PartyId, QuoteId, RequestId};
use super::money::{Amount, Price};
use super::ports::InsufficientFunds;
use super::request::{LedgerAccount, Leg, Quote, RfqRequest};
use super::state::{OracleOutcome, RequestState};

/// One-shot reply channel from the engine actor back to the caller.
pub type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

/// Everything the engine actor can be asked to do. Handlers and the expiry worker send these;
/// the actor applies them one at a time so accept and expiry cannot race.
///
/// Mutating commands reply with a snapshot of the affected aggregate so handlers can render
/// the response without a second round trip.
#[derive(Debug)]
pub enum Command {
    /// Requester opens an RFQ.
    SubmitRequest {
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
        reply: Reply<RfqRequest>,
    },
    /// Market maker quotes a leg. Reserves collateral.
    SubmitQuote {
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
        reply: Reply<Quote>,
    },
    /// Market maker cancels their own live quote while the request is still `Open`.
    CancelQuote {
        maker: PartyId,
        quote_id: QuoteId,
        reply: Reply<()>,
    },
    /// Requester accepts the presented package (`Presented → Locked`).
    Accept {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    /// Requester rejects the presented package (`Presented → Failed`).
    Reject {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    /// Oracle operator reports an outcome (`Locked | Disputed → Settled | Disputed | Unwound`).
    Resolve {
        request_id: RequestId,
        outcome: OracleOutcome,
        reply: Reply<RfqRequest>,
    },
    /// Read a request snapshot.
    GetRequest {
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    /// Mock faucet. Lives in the actor so handlers never touch the ledger directly.
    Credit {
        party: PartyId,
        amount: Amount,
        reply: Reply<LedgerAccount>,
    },
    /// Read a party's balances.
    Balance {
        party: PartyId,
        reply: Reply<LedgerAccount>,
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
    #[error("quote is no longer live")]
    QuoteNotLive,
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

impl From<super::money::InvalidPrice> for EngineError {
    fn from(_: super::money::InvalidPrice) -> Self {
        EngineError::InvalidPrice
    }
}

impl From<super::request::EmptyLegs> for EngineError {
    fn from(_: super::request::EmptyLegs) -> Self {
        EngineError::EmptyLegs
    }
}
