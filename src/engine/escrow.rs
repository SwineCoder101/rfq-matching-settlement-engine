//! Helpers shared by the transitions: the lock-batch plan built at accept, and the release
//! paths every failure funnels through.

use super::*;

/// Ledger handles for one leg's escrow, keyed by role.
#[derive(Debug, Clone, Copy)]
pub(super) struct LegEscrow {
    pub(super) yes_buyer: EscrowHandle,
    pub(super) yes_seller: EscrowHandle,
}

pub(super) fn expect_state(req: &RfqRequest, expected: RequestState) -> Result<(), EngineError> {
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
pub(super) fn escrow_plan(
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
pub(super) fn lock_selected_release_losers(
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

/// Release every quote matching `pred` that still holds collateral (`Live` or `Selected`).
pub(super) fn release_quotes(
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
pub(super) fn fail_request(
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
