//! Market-maker commands while the request is `Open`: quote and cancel.

use super::escrow::{expect_state, release_quotes};
use super::*;

impl Engine {
    pub(super) fn submit_quote(
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

    pub(super) fn cancel_quote(
        &mut self,
        maker: PartyId,
        quote_id: QuoteId,
    ) -> Result<(), EngineError> {
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
}
