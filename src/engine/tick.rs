//! The expiry worker's heartbeat: every timer in the state machine lives here.

use super::escrow::{fail_request, release_quotes};
use super::oracle::{settle, unwind};
use super::*;

impl Engine {
    pub(super) fn tick(&mut self, now: DateTime<Utc>) {
        let ledger = &*self.ledger;
        let reservations = &mut self.reservations;
        let escrows = &mut self.escrows;
        for req in self.requests.values_mut() {
            match req.state {
                RequestState::Open => {
                    release_quotes(ledger, reservations, req, |q| {
                        q.state == QuoteState::Live && q.expires_at <= now
                    });
                    if now >= req.response_deadline {
                        // The requester may never accept once the contracts have resolved,
                        // however late the worker is: the window ends at `resolves_at`.
                        let accept_deadline =
                            (now + self.config.accept_window).min(req.resolves_at);
                        present_or_fail(ledger, reservations, req, now, accept_deadline);
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
                RequestState::Reported => {
                    let deadline = req
                        .dispute_deadline
                        .expect("a Reported request always has a dispute deadline");
                    if now > deadline {
                        let outcome = req
                            .reported_outcome
                            .expect("a Reported request always has an outcome");
                        settle(ledger, escrows, req, outcome);
                    }
                }
                RequestState::Disputed => {
                    if let Some(deadline) = req.unwind_deadline
                        && now > deadline
                    {
                        unwind(ledger, escrows, req);
                    }
                }
                RequestState::Locked
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
