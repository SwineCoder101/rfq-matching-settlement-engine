//! Party balances in three buckets: free, reserved (quote-scoped), escrowed (request-scoped).
//! This is the port the engine talks to; the in-memory implementation is `crate::mock`.

use serde::Serialize;

use crate::domain::{Amount, PartyId};

/// Handle to a reversible, quote-scoped hold on a market maker's funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservationId(pub(crate) u64);

/// Handle to one party's escrowed chunk of one leg. `lock_batch` issues one per [`LockItem`],
/// so a leg's escrow is two handles: the Yes-buyer's and the Yes-seller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EscrowHandle(pub(crate) u64);

/// One side of one leg to be moved into escrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockItem {
    /// Market-maker side: convert an existing reservation into escrow.
    FromReservation(ReservationId),
    /// Requester side: take directly from free balance (the requester never reserves).
    FromFree { party: PartyId, amount: Amount },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("insufficient funds for {party}: needed {needed}, available {available}")]
pub struct InsufficientFunds {
    pub party: PartyId,
    pub needed: Amount,
    pub available: Amount,
}

/// Why `lock_batch` refused. On error no account has been touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LockBatchError {
    #[error(transparent)]
    InsufficientFunds(#[from] InsufficientFunds),
    #[error("unknown or already-consumed reservation {0:?}")]
    UnknownReservation(ReservationId),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LedgerAccount {
    pub free: Amount,
    pub reserved: Amount,
    pub escrowed: Amount,
}

impl LedgerAccount {
    pub fn total(self) -> Amount {
        self.free + self.reserved + self.escrowed
    }
}

/// Methods take `&self` so one ledger can sit behind an `Arc`; implementations synchronize
/// internally.
pub trait Ledger {
    /// Mock faucet: add `amount` to `party`'s free balance.
    fn credit(&self, party: PartyId, amount: Amount);

    /// Move `amount` from free to reserved. Fails without side effects if free is short.
    fn reserve(&self, party: PartyId, amount: Amount) -> Result<ReservationId, InsufficientFunds>;

    /// Return a reservation to free. Unknown or already-consumed handles are a no-op.
    fn release(&self, reservation: ReservationId);

    /// Move every item into escrow, or nothing at all. Returns one handle per item, in order.
    fn lock_batch(&self, items: Vec<LockItem>) -> Result<Vec<EscrowHandle>, LockBatchError>;

    /// Pay an escrowed chunk to `to`'s free balance. Consumes the handle; a repeat is a no-op.
    fn payout(&self, escrow: EscrowHandle, to: PartyId);

    /// Return an escrowed chunk to the party that posted it. Consumes the handle; a repeat is
    /// a no-op.
    fn refund(&self, escrow: EscrowHandle);

    fn balance(&self, party: PartyId) -> LedgerAccount;
}
