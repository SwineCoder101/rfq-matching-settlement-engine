//! The engine owns every `RfqRequest`, applies one [`Command`] at a time, and is the only
//! thing that touches the ledger. Split by who triggers each transition, mirroring the state
//! machine in `docs/ARCHITECTURE.md`:
//! - [`requester`]: open, accept, reject.
//! - [`maker`]: quote, cancel.
//! - [`oracle`]: resolve, dispute, and the once-only settle / unwind.
//! - [`tick`]: every timer, and presenting or failing at the response deadline.
//! - [`escrow`]: the lock-batch plan and the release helpers the above share.
//! - [`actor`]: the Tokio task that serializes it all, and the handle clients use.

mod actor;
mod command;
mod error;
mod escrow;
mod maker;
mod oracle;
mod requester;
mod tick;

pub use actor::{EngineHandle, spawn_engine};
pub(crate) use command::Command;
pub use error::EngineError;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::clock::Clock;
use crate::domain::{
    Amount, Escrow, FailReason, Leg, LegId, OracleOutcome, Package, PartyId, Price, Quote, QuoteId,
    QuoteState, RequestId, RequestState, RfqRequest, Selection, Seq, Tenor,
};
use crate::ledger::{EscrowHandle, Ledger, LockBatchError, LockItem, ReservationId};
use crate::matching::select_best;

use escrow::LegEscrow;

pub type SharedLedger = Arc<dyn Ledger + Send + Sync>;
pub type SharedClock = Arc<dyn Clock + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// How long the requester has to accept once a package is presented.
    pub accept_window: Duration,
    /// How far past the venue clock a `response_deadline` may be. Bounds how long maker
    /// collateral can sit reserved and keeps every later deadline sum representable.
    pub max_response_horizon: Duration,
    /// After the oracle reports `Yes` / `No`, how long a party may file a dispute before the
    /// reported outcome pays out.
    pub dispute_window: Duration,
    /// After a dispute is filed, how long adjudication may take before every poster is
    /// refunded instead.
    pub unwind_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            accept_window: Duration::seconds(60),
            max_response_horizon: Duration::days(365),
            dispute_window: Duration::seconds(60),
            unwind_timeout: Duration::days(1),
        }
    }
}

/// Single-threaded state machine. Exactly one task drives it.
pub struct Engine {
    ledger: SharedLedger,
    clock: SharedClock,
    config: EngineConfig,
    requests: HashMap<RequestId, RfqRequest>,
    /// Which request a quote belongs to, for `CancelQuote`.
    quote_owner: HashMap<QuoteId, RequestId>,
    /// Live/Selected quote → its collateral reservation. Removed when released or locked.
    reservations: HashMap<QuoteId, ReservationId>,
    /// Locked leg → its two escrow handles. Removed on payout or refund.
    escrows: HashMap<(RequestId, LegId), LegEscrow>,
    next_seq: Seq,
}

impl Engine {
    pub fn new(ledger: SharedLedger, clock: SharedClock, config: EngineConfig) -> Self {
        Self {
            ledger,
            clock,
            config,
            requests: HashMap::new(),
            quote_owner: HashMap::new(),
            reservations: HashMap::new(),
            escrows: HashMap::new(),
            next_seq: Seq::ZERO,
        }
    }

    pub(crate) fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::SubmitRequest {
                requester,
                legs,
                tenor,
                response_deadline,
                reply,
            } => {
                let _ = reply.send(self.submit_request(requester, legs, tenor, response_deadline));
            }
            Command::SubmitQuote {
                maker,
                request_id,
                leg_id,
                price,
                size,
                expires_at,
                reply,
            } => {
                let _ = reply
                    .send(self.submit_quote(maker, request_id, leg_id, price, size, expires_at));
            }
            Command::CancelQuote {
                maker,
                quote_id,
                reply,
            } => {
                let _ = reply.send(self.cancel_quote(maker, quote_id));
            }
            Command::Accept {
                requester,
                request_id,
                reply,
            } => {
                let _ = reply.send(self.accept(requester, request_id));
            }
            Command::Reject {
                requester,
                request_id,
                reply,
            } => {
                let _ = reply.send(self.reject(requester, request_id));
            }
            Command::Resolve {
                request_id,
                outcome,
                reply,
            } => {
                let _ = reply.send(self.resolve(request_id, outcome));
            }
            Command::Dispute {
                party,
                request_id,
                reply,
            } => {
                let _ = reply.send(self.dispute(party, request_id));
            }
            Command::GetRequest { request_id, reply } => {
                let _ = reply.send(
                    self.requests
                        .get(&request_id)
                        .cloned()
                        .ok_or(EngineError::NotFound),
                );
            }
            Command::Credit {
                party,
                amount,
                reply,
            } => {
                self.ledger.credit(party, amount);
                let _ = reply.send(Ok(self.ledger.balance(party)));
            }
            Command::Balance { party, reply } => {
                let _ = reply.send(Ok(self.ledger.balance(party)));
            }
            Command::Tick { now } => self.tick(now),
        }
    }
}
