use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::domain::{
    Amount, Clock, Command, EngineError, Escrow, EscrowHandle, FailReason, Ledger, LedgerAccount,
    Leg, LegId, LockBatchError, LockItem, OracleOutcome, Package, PartyId, Price, Quote, QuoteId,
    QuoteState, RequestId, RequestState, ReservationId, RfqRequest, Selection, Seq, select_best,
};

pub type SharedLedger = Arc<dyn Ledger + Send + Sync>;
pub type SharedClock = Arc<dyn Clock + Send + Sync>;

/// Tunables. Only the accept window exists so far; `resolution_timeout` and `unwind_timeout`
/// arrive with the delay policy.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// How long the requester has to accept once a package is presented.
    pub accept_window: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { accept_window: Duration::seconds(60) }
    }
}

/// Ledger handles for one leg's escrow, keyed by role.
#[derive(Debug, Clone, Copy)]
struct LegEscrow {
    yes_buyer: EscrowHandle,
    yes_seller: EscrowHandle,
}

/// Single-threaded state machine. Not `Sync` by design: exactly one task drives it.
pub struct Engine {
    ledger: SharedLedger,
    clock: SharedClock,
    config: EngineConfig,
    requests: HashMap<RequestId, RfqRequest>,
    /// Which request a quote belongs to, for `CancelQuote`.
    quote_owner: HashMap<QuoteId, RequestId>,
    /// Live/selected quote → its collateral reservation. Removed when released or locked.
    reservations: HashMap<QuoteId, ReservationId>,
    /// Locked leg → its two escrow handles. Removed on payout or refund.
    escrows: HashMap<(RequestId, LegId), LegEscrow>,
    next_seq: Seq,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("requests", &self.requests.len())
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
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
    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::SubmitRequest { requester, legs, response_deadline, reply } => {
                let _ = reply.send(self.submit_request(requester, legs, response_deadline));
            }
            Command::SubmitQuote { maker, request_id, leg_id, price, size, expires_at, reply } => {
                let _ = reply.send(self.submit_quote(maker, request_id, leg_id, price, size, expires_at));
            }
            Command::CancelQuote { maker, quote_id, reply } => {
                let _ = reply.send(self.cancel_quote(maker, quote_id));
            }
            Command::Accept { requester, request_id, reply } => {
                let _ = reply.send(self.accept(requester, request_id));
            }
            Command::Reject { requester, request_id, reply } => {
                let _ = reply.send(self.reject(requester, request_id));
            }
            Command::Resolve { request_id, outcome, reply } => {
                let _ = reply.send(self.resolve(request_id, outcome));
            }
            Command::GetRequest { request_id, reply } => {
                let _ = reply.send(self.requests.get(&request_id).cloned().ok_or(EngineError::NotFound));
            }
            Command::Credit { party, amount, reply } => {
                self.ledger.credit(party, amount);
                let _ = reply.send(Ok(self.ledger.balance(party)));
            }
            Command::Balance { party, reply } => {
                let _ = reply.send(Ok(self.ledger.balance(party)));
            }
            Command::Tick { now } => self.tick(now),
        }
    }

    pub fn balance(&self, party: PartyId) -> LedgerAccount {
        self.ledger.balance(party)
    }

    // -------------------------------------------------------------------------------------
    // Requester
    // -------------------------------------------------------------------------------------

    pub fn submit_request(
        &mut self,
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
    ) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        if response_deadline <= now {
            return Err(EngineError::DeadlineInPast);
        }
        let request = RfqRequest::open(RequestId::new(), requester, legs, response_deadline, now)?;
        self.requests.insert(request.id, request.clone());
        Ok(request)
    }

    pub fn accept(&mut self, requester: PartyId, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        let req = self.requests.get_mut(&request_id).ok_or(EngineError::NotFound)?;
        if req.requester != requester {
            return Err(EngineError::NotOwner);
        }
        if req.state != RequestState::Presented {
            return Err(EngineError::WrongState { expected: RequestState::Presented, actual: req.state });
        }
        if let Some(deadline) = req.accept_deadline
            && now > deadline
        {
            // The worker would have failed this on its next tick; do it now so the requester
            // cannot sneak an accept past the window.
            fail_request(&*self.ledger, &mut self.reservations, req, FailReason::AcceptWindowExpired);
            return Err(EngineError::WrongState { expected: RequestState::Presented, actual: RequestState::Failed });
        }
        let package = req.package.clone().expect("a Presented request always has a package");

        // One escrow per leg, two lock items per escrow: MM side from its reservation,
        // requester side from free balance.
        let mut items = Vec::with_capacity(package.selections.len() * 2);
        let mut escrows: Vec<(Escrow, bool)> = Vec::with_capacity(package.selections.len());
        for sel in &package.selections {
            let leg = req.leg(sel.leg_id).expect("package references a leg of this request");
            let quote = req.quote(sel.quote_id).expect("package references a quote of this request");
            let reservation = *self
                .reservations
                .get(&quote.id)
                .expect("a selected quote always holds a reservation");
            let escrow = Escrow::new(req.id, leg, quote.price, req.requester, quote.maker);
            let requester_buys_yes = leg.side.requester_buys_yes();
            let requester_amount =
                if requester_buys_yes { escrow.yes_buyer_amount } else { escrow.yes_seller_amount };
            items.push(LockItem::FromReservation(reservation));
            items.push(LockItem::FromFree { party: requester, amount: requester_amount });
            escrows.push((escrow, requester_buys_yes));
        }

        let handles = match self.ledger.lock_batch(items) {
            Ok(handles) => handles,
            Err(LockBatchError::InsufficientFunds(e)) => {
                fail_request(&*self.ledger, &mut self.reservations, req, FailReason::InsufficientRequesterFunds);
                return Err(e.into());
            }
            Err(LockBatchError::UnknownReservation(r)) => {
                unreachable!("engine invariant violated: reservation {r:?} vanished before accept")
            }
        };

        for ((escrow, requester_buys_yes), pair) in escrows.iter().zip(handles.chunks_exact(2)) {
            let (maker_handle, requester_handle) = (pair[0], pair[1]);
            let leg_escrow = if *requester_buys_yes {
                LegEscrow { yes_buyer: requester_handle, yes_seller: maker_handle }
            } else {
                LegEscrow { yes_buyer: maker_handle, yes_seller: requester_handle }
            };
            self.escrows.insert((req.id, escrow.leg_id), leg_escrow);
        }

        // Selected quotes are now locked (reservation consumed); every other quote loses.
        for quote in &mut req.quotes {
            match quote.state {
                QuoteState::Selected => {
                    quote.state = QuoteState::Locked;
                    self.reservations.remove(&quote.id);
                }
                QuoteState::Live => {
                    quote.state = QuoteState::Released;
                    if let Some(r) = self.reservations.remove(&quote.id) {
                        self.ledger.release(r);
                    }
                }
                QuoteState::Locked | QuoteState::Released => {}
            }
        }
        req.escrows = escrows.into_iter().map(|(e, _)| e).collect();
        req.state = RequestState::Locked;
        Ok(req.clone())
    }

    pub fn reject(&mut self, requester: PartyId, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        let req = self.requests.get_mut(&request_id).ok_or(EngineError::NotFound)?;
        if req.requester != requester {
            return Err(EngineError::NotOwner);
        }
        if req.state != RequestState::Presented {
            return Err(EngineError::WrongState { expected: RequestState::Presented, actual: req.state });
        }
        fail_request(&*self.ledger, &mut self.reservations, req, FailReason::Rejected);
        Ok(req.clone())
    }

    // -------------------------------------------------------------------------------------
    // Market maker
    // -------------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn submit_quote(
        &mut self,
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
    ) -> Result<Quote, EngineError> {
        let now = self.clock.now();
        let accept_window = self.config.accept_window;
        let req = self.requests.get_mut(&request_id).ok_or(EngineError::NotFound)?;
        if req.state != RequestState::Open {
            return Err(EngineError::WrongState { expected: RequestState::Open, actual: req.state });
        }
        let leg = req.leg(leg_id).ok_or(EngineError::NotFound)?;
        if expires_at <= now {
            return Err(EngineError::QuoteExpired);
        }
        if size < leg.notional {
            return Err(EngineError::QuoteTooSmall);
        }
        if expires_at < req.response_deadline + accept_window {
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
        let collateral = quote.maker_lock(leg);
        let reservation = self.ledger.reserve(maker, collateral)?;

        self.next_seq = self.next_seq.next();
        self.reservations.insert(quote.id, reservation);
        self.quote_owner.insert(quote.id, request_id);
        req.quotes.push(quote.clone());
        Ok(quote)
    }

    pub fn cancel_quote(&mut self, maker: PartyId, quote_id: QuoteId) -> Result<(), EngineError> {
        let request_id = *self.quote_owner.get(&quote_id).ok_or(EngineError::NotFound)?;
        let req = self.requests.get_mut(&request_id).expect("quote_owner only points at live requests");
        let quote = req.quote(quote_id).ok_or(EngineError::NotFound)?;
        if quote.maker != maker {
            return Err(EngineError::NotOwner);
        }
        if req.state != RequestState::Open {
            return Err(EngineError::WrongState { expected: RequestState::Open, actual: req.state });
        }
        if quote.state != QuoteState::Live {
            return Err(EngineError::QuoteNotLive);
        }
        release_quotes(&*self.ledger, &mut self.reservations, req, |q| q.id == quote_id);
        Ok(())
    }

    // -------------------------------------------------------------------------------------
    // Oracle
    // -------------------------------------------------------------------------------------

    pub fn resolve(&mut self, request_id: RequestId, outcome: OracleOutcome) -> Result<RfqRequest, EngineError> {
        let req = self.requests.get_mut(&request_id).ok_or(EngineError::NotFound)?;
        if !matches!(req.state, RequestState::Locked | RequestState::Disputed) {
            return Err(EngineError::WrongState { expected: RequestState::Locked, actual: req.state });
        }
        match outcome {
            OracleOutcome::Yes | OracleOutcome::No => {
                for escrow in &req.escrows {
                    let handles = self
                        .escrows
                        .remove(&(req.id, escrow.leg_id))
                        .expect("a Locked leg always holds escrow handles");
                    let winner = match outcome {
                        OracleOutcome::Yes => escrow.yes_buyer,
                        _ => escrow.yes_seller,
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
                        .expect("a Locked leg always holds escrow handles");
                    self.ledger.refund(handles.yes_buyer);
                    self.ledger.refund(handles.yes_seller);
                }
                req.state = RequestState::Unwound;
            }
            OracleOutcome::Disputed => req.state = RequestState::Disputed,
        }
        Ok(req.clone())
    }

    // -------------------------------------------------------------------------------------
    // Expiry worker
    // -------------------------------------------------------------------------------------

    /// Advance every request against `now`: expire stale quotes, present or fail requests at
    /// their response deadline, and fail presented requests whose accept window has closed.
    pub fn tick(&mut self, now: DateTime<Utc>) {
        let accept_window = self.config.accept_window;
        let ledger = &*self.ledger;
        let reservations = &mut self.reservations;

        for req in self.requests.values_mut() {
            match req.state {
                RequestState::Open => {
                    release_quotes(ledger, reservations, req, |q| q.state == QuoteState::Live && q.expires_at <= now);
                    if now >= req.response_deadline {
                        present_or_fail(ledger, reservations, req, now, now + accept_window);
                    }
                }
                RequestState::Presented => {
                    let deadline = req.accept_deadline.expect("a Presented request always has an accept deadline");
                    if now > deadline {
                        fail_request(ledger, reservations, req, FailReason::AcceptWindowExpired);
                    }
                }
                // Locked/Disputed delay policy (resolution_timeout, unwind_timeout) is not
                // implemented yet; terminal states need nothing.
                RequestState::Locked
                | RequestState::Disputed
                | RequestState::Settled
                | RequestState::Unwound
                | RequestState::Failed => {}
            }
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
            Some(quote_id) => selections.push(Selection { leg_id: leg.id, quote_id }),
            None => {
                let leg_id = leg.id;
                fail_request(ledger, reservations, req, FailReason::LegUnmatched(leg_id));
                return;
            }
        }
    }
    for sel in &selections {
        req.quote_mut(sel.quote_id).expect("selection came from this request").state = QuoteState::Selected;
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
    debug_assert!(req.escrows.is_empty(), "Failed is only reachable before escrow exists");
    release_quotes(ledger, reservations, req, |_| true);
    req.state = RequestState::Failed;
    req.fail_reason = Some(reason);
}
