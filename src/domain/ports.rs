//! Ports: the traits the engine talks to. Chain, payments, and the oracle are mocked behind
//! these (see `crate::mocks`).

use chrono::{DateTime, Utc};

use super::ids::{ContractId, PartyId};
use super::money::Amount;
use super::request::LedgerAccount;
use super::state::OracleOutcome;

/// Handle to a reversible, quote-scoped hold on a market maker's funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservationId(u64);

impl ReservationId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Handle to one party's escrowed chunk of one leg. `lock_batch` issues one per [`LockItem`],
/// so an [`super::Escrow`] corresponds to two handles: the Yes-buyer's and the Yes-seller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EscrowHandle(u64);

impl EscrowHandle {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// One side of one leg to be moved into escrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockItem {
    /// Market-maker side: convert an existing reservation into escrow.
    FromReservation(ReservationId),
    /// Requester side: take directly from free balance (the requester never reserves).
    FromFree { party: PartyId, amount: Amount },
}

/// A party's free balance could not cover `needed`.
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

/// Party balances in three buckets: free, reserved (quote-scoped), escrowed (request-scoped).
///
/// Methods take `&self` so one ledger can sit behind an `Arc` shared by the engine actor and
/// the faucet/balance endpoints; implementations provide their own interior synchronization.
pub trait Ledger {
    /// Mock faucet: add `amount` to `party`'s free balance.
    fn credit(&self, party: PartyId, amount: Amount);

    /// Move `amount` from free to reserved. Fails without side effects if free is short.
    fn reserve(&self, party: PartyId, amount: Amount) -> Result<ReservationId, InsufficientFunds>;

    /// Return a reservation to free. Unknown or already-consumed handles are a no-op.
    fn release(&self, reservation: ReservationId);

    /// Move every item into escrow, or nothing at all. On `Err` no account is mutated and no
    /// handle is issued. Returns one handle per item, in input order.
    fn lock_batch(&self, items: Vec<LockItem>) -> Result<Vec<EscrowHandle>, LockBatchError>;

    /// Pay an escrowed chunk to `to`'s free balance. Consumes the handle; a repeat is a no-op.
    fn payout(&self, escrow: EscrowHandle, to: PartyId);

    /// Return an escrowed chunk to the party that posted it (so refunding both handles of a
    /// leg returns `yes_buyer_amount` and `yes_seller_amount` to each poster). Consumes the
    /// handle; a repeat is a no-op.
    fn refund(&self, escrow: EscrowHandle);

    fn balance(&self, party: PartyId) -> LedgerAccount;
}

/// Resolution source. `None` means unavailable / delayed — stay `Locked`.
pub trait Oracle {
    fn outcome(&self, contract: &ContractId) -> Option<OracleOutcome>;
}

/// Time source, so deadlines can be tested deterministically.
pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}
