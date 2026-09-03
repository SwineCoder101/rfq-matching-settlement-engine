//! Oracle and party commands from `Locked` onward: resolve, dispute, and the two functions
//! that are the only places money leaves escrow.

use super::escrow::expect_state;
use super::*;

impl Engine {
    pub(super) fn resolve(
        &mut self,
        request_id: RequestId,
        outcome: OracleOutcome,
    ) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        let wrong_state = EngineError::WrongState {
            expected: RequestState::Locked,
            actual: req.state,
        };
        match (req.state, outcome) {
            (RequestState::Locked, OracleOutcome::Yes | OracleOutcome::No) => {
                req.reported_outcome = Some(outcome);
                req.dispute_deadline = Some(now + self.config.dispute_window);
                req.state = RequestState::Reported;
            }
            (RequestState::Locked, OracleOutcome::Disputed) => {
                req.unwind_deadline = Some(now + self.config.unwind_timeout);
                req.state = RequestState::Disputed;
            }
            (RequestState::Locked | RequestState::Disputed, OracleOutcome::Invalid) => {
                unwind(&*self.ledger, &mut self.escrows, req);
            }
            (RequestState::Disputed, OracleOutcome::Yes | OracleOutcome::No) => {
                settle(&*self.ledger, &mut self.escrows, req, outcome);
            }
            (RequestState::Disputed, OracleOutcome::Disputed) => {}
            _ => return Err(wrong_state),
        }
        Ok(req.clone())
    }

    pub(super) fn dispute(
        &mut self,
        party: PartyId,
        request_id: RequestId,
    ) -> Result<RfqRequest, EngineError> {
        let now = self.clock.now();
        let req = self
            .requests
            .get_mut(&request_id)
            .ok_or(EngineError::NotFound)?;
        let is_party = party == req.requester
            || req
                .quotes
                .iter()
                .any(|q| q.maker == party && q.state == QuoteState::Locked);
        if !is_party {
            return Err(EngineError::NotOwner);
        }
        expect_state(req, RequestState::Reported)?;
        req.unwind_deadline = Some(now + self.config.unwind_timeout);
        req.state = RequestState::Disputed;
        Ok(req.clone())
    }
}

/// Pay every leg's two chunks to that leg's winner and finish the request. The handles
/// are consumed, so this can only ever run once per leg.
pub(super) fn settle(
    ledger: &dyn Ledger,
    escrows: &mut HashMap<(RequestId, LegId), LegEscrow>,
    req: &mut RfqRequest,
    outcome: OracleOutcome,
) {
    for escrow in &req.escrows {
        let handles = escrows
            .remove(&(req.id, escrow.leg_id))
            .expect("Locked leg holds escrow");
        let winner = if outcome == OracleOutcome::Yes {
            escrow.yes_buyer
        } else {
            escrow.yes_seller
        };
        ledger.payout(handles.yes_buyer, winner);
        ledger.payout(handles.yes_seller, winner);
    }
    req.state = RequestState::Settled;
}

/// Refund every leg's two chunks to their posters and finish the request.
pub(super) fn unwind(
    ledger: &dyn Ledger,
    escrows: &mut HashMap<(RequestId, LegId), LegEscrow>,
    req: &mut RfqRequest,
) {
    for escrow in &req.escrows {
        let handles = escrows
            .remove(&(req.id, escrow.leg_id))
            .expect("Locked leg holds escrow");
        ledger.refund(handles.yes_buyer);
        ledger.refund(handles.yes_seller);
    }
    req.state = RequestState::Unwound;
}
