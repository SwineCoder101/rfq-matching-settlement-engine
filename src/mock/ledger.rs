//! In-memory [`Ledger`]. One mutex around all state, so `lock_batch` is trivially atomic.
//! Carries an audit trail so tests can prove conservation.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::domain::{Amount, PartyId};
use crate::ledger::{
    EscrowHandle, InsufficientFunds, Ledger, LedgerAccount, LockBatchError, LockItem, ReservationId,
};

/// A hold on one party's funds: who posted it and how much.
#[derive(Debug, Clone, Copy)]
struct Hold {
    party: PartyId,
    amount: Amount,
}

#[derive(Debug, Default)]
struct Inner {
    accounts: HashMap<PartyId, LedgerAccount>,
    reservations: HashMap<ReservationId, Hold>,
    escrows: HashMap<EscrowHandle, Hold>,
    next_handle: u64,
    /// Audit trail so tests can prove `total == credited - paid_out + received` per party.
    credited: HashMap<PartyId, Amount>,
    paid_to_others: HashMap<PartyId, Amount>,
    received_from_others: HashMap<PartyId, Amount>,
    lock_batch_calls: usize,
}

impl Inner {
    fn account(&mut self, party: PartyId) -> &mut LedgerAccount {
        self.accounts.entry(party).or_default()
    }

    fn free_of(&self, party: PartyId) -> Amount {
        self.accounts.get(&party).map_or(Amount::ZERO, |a| a.free)
    }

    fn fresh_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }
}

/// One party's row in [`MockLedger::audit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyAudit {
    pub party: PartyId,
    pub account: LedgerAccount,
    pub credited: Amount,
    pub paid_to_others: Amount,
    pub received_from_others: Amount,
}

/// In-memory [`Ledger`]. One mutex around all state, so `lock_batch` is trivially atomic.
#[derive(Debug, Default)]
pub struct MockLedger {
    inner: Mutex<Inner>,
}

impl MockLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Test helper: every unit credited is still in some account.
    pub fn conservation_holds(&self) -> bool {
        let inner = self.lock();
        let held: Amount = inner.accounts.values().map(|a| a.total()).sum();
        held == inner.credited.values().copied().sum()
    }

    /// Test helper: sum of every party's `escrowed` bucket.
    pub fn escrowed_total(&self) -> Amount {
        self.lock().accounts.values().map(|a| a.escrowed).sum()
    }

    /// Test helper: how many times `lock_batch` was attempted, refused batches included.
    pub fn lock_batch_calls(&self) -> usize {
        self.lock().lock_batch_calls
    }

    /// Test helper: one row per party that was ever credited.
    pub fn audit(&self) -> Vec<PartyAudit> {
        let inner = self.lock();
        let zero = Amount::ZERO;
        inner
            .credited
            .iter()
            .map(|(&party, &credited)| PartyAudit {
                party,
                account: inner.accounts.get(&party).copied().unwrap_or_default(),
                credited,
                paid_to_others: *inner.paid_to_others.get(&party).unwrap_or(&zero),
                received_from_others: *inner.received_from_others.get(&party).unwrap_or(&zero),
            })
            .collect()
    }
}

impl Ledger for MockLedger {
    fn credit(&self, party: PartyId, amount: Amount) {
        let mut inner = self.lock();
        inner.account(party).free += amount;
        *inner.credited.entry(party).or_insert(Amount::ZERO) += amount;
    }

    fn reserve(&self, party: PartyId, amount: Amount) -> Result<ReservationId, InsufficientFunds> {
        let mut inner = self.lock();
        let available = inner.free_of(party);
        if available < amount {
            return Err(InsufficientFunds {
                party,
                needed: amount,
                available,
            });
        }
        let account = inner.account(party);
        account.free -= amount;
        account.reserved += amount;
        let id = ReservationId(inner.fresh_handle());
        inner.reservations.insert(id, Hold { party, amount });
        Ok(id)
    }

    fn release(&self, reservation: ReservationId) {
        let mut inner = self.lock();
        let Some(hold) = inner.reservations.remove(&reservation) else {
            return;
        };
        let account = inner.account(hold.party);
        account.reserved -= hold.amount;
        account.free += hold.amount;
    }

    fn lock_batch(&self, items: Vec<LockItem>) -> Result<Vec<EscrowHandle>, LockBatchError> {
        let mut inner = self.lock();
        inner.lock_batch_calls += 1;

        // Phase 1: validate against a scratch view; nothing is mutated. Several `FromFree`
        // items for one party must be covered by that party's free balance together, and a
        // reservation may be consumed at most once per batch.
        let mut pending_free: HashMap<PartyId, Amount> = HashMap::new();
        let mut consumed: Vec<ReservationId> = Vec::new();
        let mut holds: Vec<Hold> = Vec::with_capacity(items.len());
        for item in &items {
            match *item {
                LockItem::FromReservation(id) => {
                    if consumed.contains(&id) {
                        return Err(LockBatchError::UnknownReservation(id));
                    }
                    let hold = inner
                        .reservations
                        .get(&id)
                        .copied()
                        .ok_or(LockBatchError::UnknownReservation(id))?;
                    consumed.push(id);
                    holds.push(hold);
                }
                LockItem::FromFree { party, amount } => {
                    let needed = pending_free.entry(party).or_insert(Amount::ZERO);
                    // A sum past u64::MAX can never be covered; saturate and let the check fail.
                    *needed = needed.checked_add(amount).unwrap_or(Amount::new(u64::MAX));
                    let available = inner.free_of(party);
                    if available < *needed {
                        return Err(InsufficientFunds {
                            party,
                            needed: *needed,
                            available,
                        }
                        .into());
                    }
                    holds.push(Hold { party, amount });
                }
            }
        }

        // Phase 2: apply. Every check above passed, so none of these can fail.
        let mut handles = Vec::with_capacity(items.len());
        for (item, hold) in items.iter().zip(holds) {
            match *item {
                LockItem::FromReservation(id) => {
                    inner.reservations.remove(&id);
                    let account = inner.account(hold.party);
                    account.reserved -= hold.amount;
                    account.escrowed += hold.amount;
                }
                LockItem::FromFree { .. } => {
                    let account = inner.account(hold.party);
                    account.free -= hold.amount;
                    account.escrowed += hold.amount;
                }
            }
            let handle = EscrowHandle(inner.fresh_handle());
            inner.escrows.insert(handle, hold);
            handles.push(handle);
        }
        Ok(handles)
    }

    fn payout(&self, escrow: EscrowHandle, to: PartyId) {
        let mut inner = self.lock();
        let Some(hold) = inner.escrows.remove(&escrow) else {
            return;
        };
        inner.account(hold.party).escrowed -= hold.amount;
        inner.account(to).free += hold.amount;
        if to != hold.party {
            *inner
                .paid_to_others
                .entry(hold.party)
                .or_insert(Amount::ZERO) += hold.amount;
            *inner.received_from_others.entry(to).or_insert(Amount::ZERO) += hold.amount;
        }
    }

    fn refund(&self, escrow: EscrowHandle) {
        let mut inner = self.lock();
        let Some(hold) = inner.escrows.remove(&escrow) else {
            return;
        };
        let account = inner.account(hold.party);
        account.escrowed -= hold.amount;
        account.free += hold.amount;
    }

    fn balance(&self, party: PartyId) -> LedgerAccount {
        self.lock()
            .accounts
            .get(&party)
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(free: u64, reserved: u64, escrowed: u64) -> LedgerAccount {
        LedgerAccount {
            free: Amount::new(free),
            reserved: Amount::new(reserved),
            escrowed: Amount::new(escrowed),
        }
    }

    #[test]
    fn ledger_account_total() {
        assert_eq!(acct(1, 2, 3).total(), Amount::new(6));
    }

    #[test]
    fn credit_and_balance() {
        let ledger = MockLedger::new();
        let p = PartyId::new();
        assert_eq!(ledger.balance(p), acct(0, 0, 0));
        ledger.credit(p, Amount::new(100));
        ledger.credit(p, Amount::new(50));
        assert_eq!(ledger.balance(p), acct(150, 0, 0));
    }

    #[test]
    fn reserve_then_release_round_trips() {
        let ledger = MockLedger::new();
        let p = PartyId::new();
        ledger.credit(p, Amount::new(100));

        let r = ledger.reserve(p, Amount::new(40)).unwrap();
        assert_eq!(ledger.balance(p), acct(60, 40, 0));

        assert_eq!(
            ledger.reserve(p, Amount::new(61)),
            Err(InsufficientFunds {
                party: p,
                needed: Amount::new(61),
                available: Amount::new(60)
            })
        );
        assert_eq!(
            ledger.balance(p),
            acct(60, 40, 0),
            "failed reserve has no side effects"
        );

        ledger.release(r);
        assert_eq!(ledger.balance(p), acct(100, 0, 0));
        ledger.release(r);
        assert_eq!(
            ledger.balance(p),
            acct(100, 0, 0),
            "double release is a no-op"
        );
        assert!(ledger.conservation_holds());
    }

    #[test]
    fn lock_batch_is_all_or_nothing() {
        let ledger = MockLedger::new();
        let mm = PartyId::new();
        let requester = PartyId::new();
        let poor = PartyId::new();
        ledger.credit(mm, Amount::new(100));
        ledger.credit(requester, Amount::new(100));
        ledger.credit(poor, Amount::new(10));

        let reservation = ledger.reserve(mm, Amount::new(40)).unwrap();
        let before = [
            ledger.balance(mm),
            ledger.balance(requester),
            ledger.balance(poor),
        ];
        assert_eq!(before, [acct(60, 40, 0), acct(100, 0, 0), acct(10, 0, 0)]);

        // Third item cannot be covered: `poor` has 10 free, needs 50.
        let result = ledger.lock_batch(vec![
            LockItem::FromReservation(reservation),
            LockItem::FromFree {
                party: requester,
                amount: Amount::new(60),
            },
            LockItem::FromFree {
                party: poor,
                amount: Amount::new(50),
            },
        ]);
        assert_eq!(
            result,
            Err(LockBatchError::InsufficientFunds(InsufficientFunds {
                party: poor,
                needed: Amount::new(50),
                available: Amount::new(10),
            }))
        );

        let after = [
            ledger.balance(mm),
            ledger.balance(requester),
            ledger.balance(poor),
        ];
        assert_eq!(after, before, "no account may change when any item fails");
        assert!(ledger.lock().escrows.is_empty(), "no escrow handles issued");
        assert!(
            ledger.lock().reservations.contains_key(&reservation),
            "the MM reservation is still intact"
        );
        assert!(ledger.conservation_holds());
    }

    #[test]
    fn lock_batch_sums_multiple_free_items_for_one_party() {
        let ledger = MockLedger::new();
        let p = PartyId::new();
        ledger.credit(p, Amount::new(100));

        // Each item alone fits; together they do not.
        let result = ledger.lock_batch(vec![
            LockItem::FromFree {
                party: p,
                amount: Amount::new(60),
            },
            LockItem::FromFree {
                party: p,
                amount: Amount::new(60),
            },
        ]);
        assert!(matches!(result, Err(LockBatchError::InsufficientFunds(_))));
        assert_eq!(ledger.balance(p), acct(100, 0, 0));
    }

    #[test]
    fn lock_batch_rejects_unknown_and_reused_reservations() {
        let ledger = MockLedger::new();
        let p = PartyId::new();
        ledger.credit(p, Amount::new(100));
        let r = ledger.reserve(p, Amount::new(10)).unwrap();

        let bogus = ReservationId(9_999);
        assert_eq!(
            ledger.lock_batch(vec![LockItem::FromReservation(bogus)]),
            Err(LockBatchError::UnknownReservation(bogus))
        );
        assert_eq!(
            ledger.lock_batch(vec![
                LockItem::FromReservation(r),
                LockItem::FromReservation(r)
            ]),
            Err(LockBatchError::UnknownReservation(r))
        );
        assert_eq!(ledger.balance(p), acct(90, 10, 0));
    }

    #[test]
    fn happy_path_lock_payout_and_refund() {
        let ledger = MockLedger::new();
        let mm = PartyId::new();
        let requester = PartyId::new();
        ledger.credit(mm, Amount::new(100));
        ledger.credit(requester, Amount::new(100));

        // Leg: notional 100 at p = 40%. Requester buys Yes (locks 40), MM sells Yes (locks 60).
        let reservation = ledger.reserve(mm, Amount::new(60)).unwrap();
        let handles = ledger
            .lock_batch(vec![
                LockItem::FromReservation(reservation),
                LockItem::FromFree {
                    party: requester,
                    amount: Amount::new(40),
                },
            ])
            .unwrap();
        assert_eq!(handles.len(), 2);
        assert_eq!(ledger.balance(mm), acct(40, 0, 60));
        assert_eq!(ledger.balance(requester), acct(60, 0, 40));
        assert!(
            ledger.lock().reservations.is_empty(),
            "reservation consumed by lock"
        );

        // Yes resolves: requester wins n = 100.
        for h in &handles {
            ledger.payout(*h, requester);
        }
        assert_eq!(ledger.balance(mm), acct(40, 0, 0));
        assert_eq!(ledger.balance(requester), acct(160, 0, 0));
        ledger.payout(handles[0], requester);
        assert_eq!(
            ledger.balance(requester),
            acct(160, 0, 0),
            "double payout is a no-op"
        );
        assert!(ledger.conservation_holds());

        // Fresh leg, then Invalid: refund returns each side to its poster.
        let reservation = ledger.reserve(mm, Amount::new(30)).unwrap();
        let handles = ledger
            .lock_batch(vec![
                LockItem::FromReservation(reservation),
                LockItem::FromFree {
                    party: requester,
                    amount: Amount::new(70),
                },
            ])
            .unwrap();
        assert_eq!(ledger.balance(mm), acct(10, 0, 30));
        assert_eq!(ledger.balance(requester), acct(90, 0, 70));
        for h in &handles {
            ledger.refund(*h);
        }
        assert_eq!(ledger.balance(mm), acct(40, 0, 0));
        assert_eq!(ledger.balance(requester), acct(160, 0, 0));
        assert!(ledger.conservation_holds());
        assert_eq!(ledger.escrowed_total(), Amount::ZERO);
    }
}
