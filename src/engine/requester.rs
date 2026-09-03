//! Requester commands: `[*] → Open`, `Presented → Locked | Failed`.

use super::escrow::{escrow_plan, expect_state, fail_request, lock_selected_release_losers};
use super::*;

impl Engine {
    pub(super) fn submit_request(
        &mut self,
        requester: PartyId,
        legs: Vec<Leg>,
        tenor: Tenor,
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
        let resolves_at = response_deadline.checked_add_signed(tenor.duration());
        let Some(resolves_at) = resolves_at.filter(|_| within_horizon && summable) else {
            return Err(EngineError::DeadlineBeyondHorizon);
        };
        let request = RfqRequest::open(
            RequestId::new(),
            requester,
            legs,
            tenor,
            response_deadline,
            resolves_at,
            now,
        )?;
        self.requests.insert(request.id, request.clone());
        Ok(request)
    }

    pub(super) fn accept(
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

    pub(super) fn reject(
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
}
