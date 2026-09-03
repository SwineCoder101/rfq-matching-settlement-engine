//! The engine owns every `RfqRequest`, applies one [`Command`] at a time, and is the only
//! thing that touches the ledger. [`spawn_engine`] runs it on a Tokio task behind an mpsc so
//! accept, cancel, and expiry ticks are serialized and cannot race.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::domain::{
    Amount, EmptyLegs, Escrow, FailReason, Leg, LegId, OracleOutcome, Package, PartyId, Price,
    Quote, QuoteId, QuoteState, RequestId, RequestState, RfqRequest, Selection, Seq,
};
use crate::ledger::LedgerAccount;
use crate::ledger::{
    EscrowHandle, InsufficientFunds, Ledger, LockBatchError, LockItem, ReservationId,
};
use crate::matching::select_best;

pub type SharedLedger = Arc<dyn Ledger + Send + Sync>;
pub type SharedClock = Arc<dyn Clock + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// How long the requester has to accept once a package is presented.
    pub accept_window: Duration,
    /// How far past the venue clock a `response_deadline` may be. Bounds how long maker
    /// collateral can sit reserved and keeps every later deadline sum representable.
    pub max_response_horizon: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            accept_window: Duration::seconds(60),
            max_response_horizon: Duration::days(365),
        }
    }
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

type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

/// Everything the engine can be asked to do. Mutating commands reply with a snapshot of the
/// affected aggregate so handlers can render the response without a second round trip.
#[derive(Debug)]
pub(crate) enum Command {
    SubmitRequest {
        requester: PartyId,
        legs: Vec<Leg>,
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

/// Ledger handles for one leg's escrow, keyed by role.
#[derive(Debug, Clone, Copy)]
struct LegEscrow {
    yes_buyer: EscrowHandle,
    yes_seller: EscrowHandle,
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

    /// Apply one command and send its reply. A dropped receiver (client went away) is fine.
    pub(crate) fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::SubmitRequest {
                requester,
                legs,
                response_deadline,
                reply,
            } => {
                let _ = reply.send(self.submit_request(requester, legs, response_deadline));
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

    /// `[*] → Open`.
    fn submit_request(
        &mut self,
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
    ) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        if response_deadline <= now {
            return Err(EngineError::DeadlineInPast);
        }
        // Admission is the one place deadline arithmetic is checked. A stored request's
        // deadline is within the horizon and `deadline + accept_window` is representable, so
        // the plain additions in `submit_quote` and `tick` cannot overflow.
        let horizon = now.checked_add_signed(self.config.max_response_horizon);
        let within_horizon = horizon.is_some_and(|h| response_deadline <= h);
        let summable = response_deadline
            .checked_add_signed(self.config.accept_window)
            .is_some();
        if !within_horizon || !summable {
            return Err(EngineError::DeadlineBeyondHorizon);
        }
        let request = RfqRequest::open(RequestId::new(), requester, legs, response_deadline, now)?;
        self.requests.insert(request.id, request.clone());
        Ok(request)
    }

    /// `Open → Open`: a new `Live` quote with the maker's collateral reserved.
    fn submit_quote(
        &mut self,
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
    ) -> Result<Quote, EngineError> {
        let now = self.clock.now();
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        expect_state(req, RequestState::Open)?;
        let leg = req.leg(leg_id).ok_or(EngineError::NotFound)?;
        if expires_at <= now {
            return Err(EngineError::QuoteExpired);
        }
        if size < leg.notional {
            return Err(EngineError::QuoteTooSmall);
        }
        if expires_at < req.response_deadline + self.config.accept_window {
            return Err(EngineError::QuoteExpiresBeforeAcceptWindow);
        }

        let quote = Quote {
            id: QuoteId::new(),
            leg_id,
            maker,
            price,
            size,
            expires_at,
            submitted_at: now,
            seq: self.next_seq,
            state: QuoteState::Live,
        };
        let reservation = self.ledger.reserve(maker, quote.maker_lock(leg))?;

        self.next_seq = self.next_seq.next();
        self.reservations.insert(quote.id, reservation);
        self.quote_owner.insert(quote.id, request_id);
        req.quotes.push(quote.clone());
        Ok(quote)
    }

    /// `Open → Open`: `Live → Released` for the maker's own quote.
    fn cancel_quote(&mut self, maker: PartyId, quote_id: QuoteId) -> Result<(), EngineError> {
        let request_id = *self
            .quote_owner
            .get(&quote_id)
            .ok_or(EngineError::NotFound)?;
        let req = self
            .requests
            .get_mut(&request_id)
            .expect("quote_owner only points at stored requests");
        let quote = req.quote(quote_id).ok_or(EngineError::NotFound)?;
        if quote.maker != maker {
            return Err(EngineError::NotOwner);
        }
        expect_state(req, RequestState::Open)?;
        if quote.state != QuoteState::Live {
            return Err(EngineError::QuoteNotLive);
        }
        release_quotes(&*self.ledger, &mut self.reservations, req, |q| {
            q.id == quote_id
        });
        Ok(())
    }

    /// `Presented → Locked`, or `Presented → Failed` if the window has closed or the
    /// requester cannot fund its side.
    fn accept(
        &mut self,
        requester: PartyId,
        request_id: RequestId,
    ) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        if req.requester != requester {
            return Err(EngineError::NotOwner);
        }
        expect_state(req, RequestState::Presented)?;
        if now
            > req
                .accept_deadline
                .expect("a Presented request always has an accept deadline")
        {
            // Same outcome the next Tick would produce; done here so an accept cannot slip past
            // the window between ticks.
            fail_request(
                &*self.ledger,
                &mut self.reservations,
                req,
                FailReason::AcceptWindowExpired,
            );
            return Err(EngineError::WrongState {
                expected: RequestState::Presented,
                actual: RequestState::Failed,
            });
        }

        let (escrows, items) = escrow_plan(req, &self.reservations);
        let handles = match self.ledger.lock_batch(items) {
            Ok(handles) => handles,
            Err(LockBatchError::InsufficientFunds(e)) => {
                fail_request(
                    &*self.ledger,
                    &mut self.reservations,
                    req,
                    FailReason::InsufficientRequesterFunds,
                );
                return Err(e.into());
            }
            Err(LockBatchError::UnknownReservation(r)) => {
                unreachable!("engine invariant violated: reservation {r:?} vanished before accept")
            }
        };
        for (escrow, handles) in escrows.iter().zip(handles.chunks_exact(2)) {
            let (maker_handle, requester_handle) = (handles[0], handles[1]);
            let leg = req
                .leg(escrow.leg_id)
                .expect("escrow was built from this request's legs");
            let leg_escrow = if leg.side.requester_buys_yes() {
                LegEscrow {
                    yes_buyer: requester_handle,
                    yes_seller: maker_handle,
                }
            } else {
                LegEscrow {
                    yes_buyer: maker_handle,
                    yes_seller: requester_handle,
                }
            };
            self.escrows.insert((req.id, escrow.leg_id), leg_escrow);
        }
        lock_selected_release_losers(&*self.ledger, &mut self.reservations, req);
        req.escrows = escrows;
        req.state = RequestState::Locked;
        Ok(req.clone())
    }

    /// `Presented → Failed(rejected)`.
    fn reject(
        &mut self,
        requester: PartyId,
        request_id: RequestId,
    ) -> Result<RfqRequest, EngineError> {
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        if req.requester != requester {
            return Err(EngineError::NotOwner);
        }
        expect_state(req, RequestState::Presented)?;
        fail_request(
            &*self.ledger,
            &mut self.reservations,
            req,
            FailReason::Rejected,
        );
        Ok(req.clone())
    }

    /// `Locked | Disputed → Settled | Unwound | Disputed`.
    fn resolve(
        &mut self,
        request_id: RequestId,
        outcome: OracleOutcome,
    ) -> Result<RfqRequest, EngineError> {
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        if !matches!(req.state, RequestState::Locked | RequestState::Disputed) {
            return Err(EngineError::WrongState {
                expected: RequestState::Locked,
                actual: req.state,
            });
        }
        match outcome {
            OracleOutcome::Yes | OracleOutcome::No => {
                for escrow in &req.escrows {
                    let handles = self
                        .escrows
                        .remove(&(req.id, escrow.leg_id))
                        .expect("Locked leg holds escrow");
                    let winner = if outcome == OracleOutcome::Yes {
                        escrow.yes_buyer
                    } else {
                        escrow.yes_seller
                    };
                    self.ledger.payout(handles.yes_buyer, winner);
                    self.ledger.payout(handles.yes_seller, winner);
                }
                req.state = RequestState::Settled;
            }
            OracleOutcome::Invalid => {
                for escrow in &req.escrows {
                    let handles = self
                        .escrows
                        .remove(&(req.id, escrow.leg_id))
                        .expect("Locked leg holds escrow");
                    self.ledger.refund(handles.yes_buyer);
                    self.ledger.refund(handles.yes_seller);
                }
                req.state = RequestState::Unwound;
            }
            OracleOutcome::Disputed => req.state = RequestState::Disputed,
        }
        Ok(req.clone())
    }

    /// `Open → Presented | Failed` at the response deadline; `Presented → Failed` once the
    /// accept window closes. Expired `Live` quotes are released first so they cannot be
    /// selected. Locked and Disputed have no timer.
    fn tick(&mut self, now: DateTime<Utc>) {
        let ledger = &*self.ledger;
        let reservations = &mut self.reservations;
        for req in self.requests.values_mut() {
            match req.state {
                RequestState::Open => {
                    release_quotes(ledger, reservations, req, |q| {
                        q.state == QuoteState::Live && q.expires_at <= now
                    });
                    if now >= req.response_deadline {
                        present_or_fail(
                            ledger,
                            reservations,
                            req,
                            now,
                            now + self.config.accept_window,
                        );
                    }
                }
                RequestState::Presented => {
                    if now
                        > req
                            .accept_deadline
                            .expect("a Presented request always has an accept deadline")
                    {
                        fail_request(ledger, reservations, req, FailReason::AcceptWindowExpired);
                    }
                }
                RequestState::Locked
                | RequestState::Disputed
                | RequestState::Settled
                | RequestState::Unwound
                | RequestState::Failed => {}
            }
        }
    }
}

fn expect_state(req: &RfqRequest, expected: RequestState) -> Result<(), EngineError> {
    if req.state == expected {
        Ok(())
    } else {
        Err(EngineError::WrongState {
            expected,
            actual: req.state,
        })
    }
}

/// The escrow each selected leg will hold, and the lock items that fund it: two per leg, the
/// maker's reservation first and the requester's free balance second.
fn escrow_plan(
    req: &RfqRequest,
    reservations: &HashMap<QuoteId, ReservationId>,
) -> (Vec<Escrow>, Vec<LockItem>) {
    let package = req
        .package
        .as_ref()
        .expect("a Presented request always has a package");
    let mut escrows = Vec::with_capacity(package.selections.len());
    let mut items = Vec::with_capacity(package.selections.len() * 2);
    for Selection { leg_id, quote_id } in &package.selections {
        let leg = req
            .leg(*leg_id)
            .expect("package references a leg of this request");
        let quote = req
            .quote(*quote_id)
            .expect("package references a quote of this request");
        let reservation = reservations[&quote.id];
        let escrow = Escrow::new(leg, quote.price, req.requester, quote.maker);
        let requester_amount = if leg.side.requester_buys_yes() {
            escrow.yes_buyer_amount
        } else {
            escrow.yes_seller_amount
        };
        items.push(LockItem::FromReservation(reservation));
        items.push(LockItem::FromFree {
            party: req.requester,
            amount: requester_amount,
        });
        escrows.push(escrow);
    }
    (escrows, items)
}

/// After a successful `lock_batch`: `Selected → Locked` (reservation consumed by the batch),
/// every remaining `Live` quote `→ Released`.
fn lock_selected_release_losers(
    ledger: &dyn Ledger,
    reservations: &mut HashMap<QuoteId, ReservationId>,
    req: &mut RfqRequest,
) {
    for quote in &mut req.quotes {
        match quote.state {
            QuoteState::Selected => {
                quote.state = QuoteState::Locked;
                reservations.remove(&quote.id);
            }
            QuoteState::Live => {
                quote.state = QuoteState::Released;
                if let Some(r) = reservations.remove(&quote.id) {
                    ledger.release(r);
                }
            }
            QuoteState::Locked | QuoteState::Released => {}
        }
    }
}

/// `Open → Presented` if every leg has an eligible best quote, else `Open → Failed` with every
/// reservation released. The requester never sees a partial package.
fn present_or_fail(
    ledger: &dyn Ledger,
    reservations: &mut HashMap<QuoteId, ReservationId>,
    req: &mut RfqRequest,
    now: DateTime<Utc>,
    accept_deadline: DateTime<Utc>,
) {
    let mut selections = Vec::with_capacity(req.legs.len());
    for leg in &req.legs {
        match select_best(leg, &req.quotes, now, accept_deadline) {
            Some(quote_id) => selections.push(Selection {
                leg_id: leg.id,
                quote_id,
            }),
            None => {
                let leg_id = leg.id;
                fail_request(ledger, reservations, req, FailReason::LegUnmatched(leg_id));
                return;
            }
        }
    }
    for sel in &selections {
        req.quote_mut(sel.quote_id)
            .expect("selection came from this request")
            .state = QuoteState::Selected;
    }
    req.package = Some(Package { selections });
    req.accept_deadline = Some(accept_deadline);
    req.state = RequestState::Presented;
}

/// Release every quote matching `pred` that still holds collateral (`Live` or `Selected`).
fn release_quotes(
    ledger: &dyn Ledger,
    reservations: &mut HashMap<QuoteId, ReservationId>,
    req: &mut RfqRequest,
    pred: impl Fn(&Quote) -> bool,
) {
    for quote in req.quotes.iter_mut().filter(|q| pred(q)) {
        if matches!(quote.state, QuoteState::Live | QuoteState::Selected) {
            quote.state = QuoteState::Released;
            if let Some(r) = reservations.remove(&quote.id) {
                ledger.release(r);
            }
        }
    }
}

/// Move a pre-`Locked` request to `Failed`, returning every hold to its poster.
fn fail_request(
    ledger: &dyn Ledger,
    reservations: &mut HashMap<QuoteId, ReservationId>,
    req: &mut RfqRequest,
    reason: FailReason,
) {
    debug_assert!(
        req.escrows.is_empty(),
        "Failed is only reachable before escrow exists"
    );
    release_quotes(ledger, reservations, req, |_| true);
    req.state = RequestState::Failed;
    req.fail_reason = Some(reason);
}

// ---------------------------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------------------------

/// Cloneable client for the engine task. Every method sends one [`Command`] and awaits its
/// one-shot reply.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
}

/// Run `engine` on its own task. The queue is bounded so a flood of requests applies
/// back-pressure to handlers instead of growing memory; the actor exits when the last handle
/// is dropped.
pub fn spawn_engine(mut engine: Engine) -> (EngineHandle, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Command>(256);
    let task = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            engine.handle(cmd);
        }
    });
    (EngineHandle { tx }, task)
}

impl EngineHandle {
    async fn ask<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(build(reply))
            .await
            .map_err(|_| EngineError::Unavailable)?;
        rx.await.map_err(|_| EngineError::Unavailable)?
    }

    pub async fn submit_request(
        &self,
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
    ) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::SubmitRequest {
            requester,
            legs,
            response_deadline,
            reply,
        })
        .await
    }

    pub async fn submit_quote(
        &self,
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
    ) -> Result<Quote, EngineError> {
        self.ask(|reply| Command::SubmitQuote {
            maker,
            request_id,
            leg_id,
            price,
            size,
            expires_at,
            reply,
        })
        .await
    }

    pub async fn cancel_quote(&self, maker: PartyId, quote_id: QuoteId) -> Result<(), EngineError> {
        self.ask(|reply| Command::CancelQuote {
            maker,
            quote_id,
            reply,
        })
        .await
    }

    pub async fn accept(
        &self,
        requester: PartyId,
        request_id: RequestId,
    ) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Accept {
            requester,
            request_id,
            reply,
        })
        .await
    }

    pub async fn reject(
        &self,
        requester: PartyId,
        request_id: RequestId,
    ) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Reject {
            requester,
            request_id,
            reply,
        })
        .await
    }

    pub async fn resolve(
        &self,
        request_id: RequestId,
        outcome: OracleOutcome,
    ) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Resolve {
            request_id,
            outcome,
            reply,
        })
        .await
    }

    pub async fn get_request(&self, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::GetRequest { request_id, reply })
            .await
    }

    pub async fn credit(
        &self,
        party: PartyId,
        amount: Amount,
    ) -> Result<LedgerAccount, EngineError> {
        self.ask(|reply| Command::Credit {
            party,
            amount,
            reply,
        })
        .await
    }

    pub async fn balance(&self, party: PartyId) -> Result<LedgerAccount, EngineError> {
        self.ask(|reply| Command::Balance { party, reply }).await
    }

    /// Fire-and-forget heartbeat. Commands are processed in order, so anything sent after
    /// this observes its effects.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<(), EngineError> {
        self.tx
            .send(Command::Tick { now })
            .await
            .map_err(|_| EngineError::Unavailable)
    }
}
