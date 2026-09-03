//! Everything the engine can be asked to do, and the one-shot reply each carries.

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

use super::EngineError;
use crate::domain::{
    Amount, Leg, LegId, OracleOutcome, PartyId, Price, Quote, QuoteId, RequestId, RfqRequest, Tenor,
};
use crate::ledger::LedgerAccount;

pub(super) type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

/// Everything the engine can be asked to do. Mutating commands reply with a snapshot of the
/// affected aggregate so handlers can render the response without a second round trip.
#[derive(Debug)]
pub(crate) enum Command {
    SubmitRequest {
        requester: PartyId,
        legs: Vec<Leg>,
        tenor: Tenor,
        response_deadline: DateTime<Utc>,
        reply: Reply<RfqRequest>,
    },
    SubmitQuote {
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
        reply: Reply<Quote>,
    },
    CancelQuote {
        maker: PartyId,
        quote_id: QuoteId,
        reply: Reply<()>,
    },
    Accept {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    Reject {
        requester: PartyId,
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    Resolve {
        request_id: RequestId,
        outcome: OracleOutcome,
        reply: Reply<RfqRequest>,
    },
    Dispute {
        party: PartyId,
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    GetRequest {
        request_id: RequestId,
        reply: Reply<RfqRequest>,
    },
    Credit {
        party: PartyId,
        amount: Amount,
        reply: Reply<LedgerAccount>,
    },
    Balance {
        party: PartyId,
        reply: Reply<LedgerAccount>,
    },
    /// Expiry worker heartbeat. Deadlines are absolute, so `now` is carried, never read: the
    /// worker's view of time is what gets applied, and tests can hand in any instant.
    Tick { now: DateTime<Utc> },
}
