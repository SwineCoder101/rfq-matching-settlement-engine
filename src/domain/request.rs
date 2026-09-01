//! Aggregates: `RfqRequest` (root), `Leg`, `Quote`, `Package`, `Escrow`, `LedgerAccount`.

use chrono::{DateTime, Utc};

use super::ids::{ContractId, LegId, PartyId, QuoteId, RequestId, Seq};
use super::money::{Amount, Price};
use super::state::{FailReason, LegSide, QuoteState, RequestState};

/// A leg's notional must be strictly positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("leg notional must be greater than zero")]
pub struct ZeroNotional;

/// A request needs at least one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a request must have at least one leg")]
pub struct EmptyLegs;

/// One binary contract, the requester's side, and a notional. Not an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    pub id: LegId,
    pub contract: ContractId,
    pub side: LegSide,
    pub notional: Amount,
}

impl Leg {
    pub fn new(contract: ContractId, side: LegSide, notional: Amount) -> Result<Self, ZeroNotional> {
        if notional.is_zero() {
            return Err(ZeroNotional);
        }
        Ok(Self { id: LegId::new(), contract, side, notional })
    }
}

/// A market maker's firm quote on one leg. Reserves MM collateral while `Live` or `Selected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub id: QuoteId,
    pub leg_id: LegId,
    pub maker: PartyId,
    pub price: Price,
    pub size: Amount,
    pub expires_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    /// Engine-assigned, monotonic. Tie-breaker in matching.
    pub seq: Seq,
    pub state: QuoteState,
}

impl Quote {
    /// What the market maker must reserve at submit: the MM's side of the escrow at this
    /// quote's price for the leg's full notional.
    ///
    /// The MM takes the opposite side to the requester, so on a `BuyYes` leg the MM is the
    /// Yes-seller and on a `SellYes` leg the MM is the Yes-buyer.
    pub fn maker_lock(&self, leg: &Leg) -> Amount {
        debug_assert_eq!(self.leg_id, leg.id, "maker_lock called with a quote from another leg");
        if leg.side.requester_buys_yes() {
            self.price.yes_seller_lock(leg.notional)
        } else {
            self.price.yes_buyer_lock(leg.notional)
        }
    }
}

/// The best quote chosen for one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub leg_id: LegId,
    pub quote_id: QuoteId,
}

/// One selection per leg, shown to the requester once the request is `Presented`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Package {
    pub selections: Vec<Selection>,
}

/// Funds locked for one leg after accept. Exists only from `Locked` onward.
///
/// Yes-buyer locks `p * n`, Yes-seller locks `(1 - p) * n`, total `n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escrow {
    pub request_id: RequestId,
    pub leg_id: LegId,
    pub yes_buyer: PartyId,
    pub yes_seller: PartyId,
    pub yes_buyer_amount: Amount,
    pub yes_seller_amount: Amount,
    pub notional: Amount,
}

impl Escrow {
    /// Derive the escrow for `leg` at `price`. `LegSide::BuyYes` → requester buys Yes;
    /// `LegSide::SellYes` → maker buys Yes.
    pub fn new(
        request_id: RequestId,
        leg: &Leg,
        price: Price,
        requester: PartyId,
        maker: PartyId,
    ) -> Self {
        let (yes_buyer, yes_seller) = if leg.side.requester_buys_yes() {
            (requester, maker)
        } else {
            (maker, requester)
        };
        let yes_buyer_amount = price.yes_buyer_lock(leg.notional);
        let yes_seller_amount = price.yes_seller_lock(leg.notional);
        debug_assert_eq!(
            yes_buyer_amount + yes_seller_amount,
            leg.notional,
            "escrow legs must sum to notional"
        );
        Self {
            request_id,
            leg_id: leg.id,
            yes_buyer,
            yes_seller,
            yes_buyer_amount,
            yes_seller_amount,
            notional: leg.notional,
        }
    }
}

/// A party's balances. Three buckets, never mixed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

/// Aggregate root. Owns legs, quotes, deadlines, package, escrows, and `RequestState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RfqRequest {
    pub id: RequestId,
    pub requester: PartyId,
    pub legs: Vec<Leg>,
    pub quotes: Vec<Quote>,
    /// Absolute. At this instant the worker either presents a package or fails the request.
    pub response_deadline: DateTime<Utc>,
    /// Set when the request becomes `Presented`.
    pub accept_deadline: Option<DateTime<Utc>>,
    pub state: RequestState,
    /// Set when the request becomes `Presented`.
    pub package: Option<Package>,
    /// Non-empty only from `Locked` onward.
    pub escrows: Vec<Escrow>,
    pub fail_reason: Option<FailReason>,
    pub created_at: DateTime<Utc>,
}

impl RfqRequest {
    /// A fresh `Open` request with no quotes, package, or escrows.
    pub fn open(
        id: RequestId,
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
        created_at: DateTime<Utc>,
    ) -> Result<Self, EmptyLegs> {
        if legs.is_empty() {
            return Err(EmptyLegs);
        }
        Ok(Self {
            id,
            requester,
            legs,
            quotes: Vec::new(),
            response_deadline,
            accept_deadline: None,
            state: RequestState::Open,
            package: None,
            escrows: Vec::new(),
            fail_reason: None,
            created_at,
        })
    }

    pub fn leg(&self, id: LegId) -> Option<&Leg> {
        self.legs.iter().find(|l| l.id == id)
    }

    pub fn quote(&self, id: QuoteId) -> Option<&Quote> {
        self.quotes.iter().find(|q| q.id == id)
    }

    pub fn quote_mut(&mut self, id: QuoteId) -> Option<&mut Quote> {
        self.quotes.iter_mut().find(|q| q.id == id)
    }

    /// Quotes on `leg_id`, in whatever order they were stored.
    pub fn quotes_for_leg(&self, leg_id: LegId) -> impl Iterator<Item = &Quote> {
        self.quotes.iter().filter(move |q| q.leg_id == leg_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn leg(side: LegSide, notional: u64) -> Leg {
        Leg::new(ContractId::new("C").unwrap(), side, Amount::new(notional)).unwrap()
    }

    fn quote(leg: &Leg, bps: u32) -> Quote {
        Quote {
            id: QuoteId::new(),
            leg_id: leg.id,
            maker: PartyId::new(),
            price: Price::new(bps).unwrap(),
            size: leg.notional,
            expires_at: t(100),
            submitted_at: t(0),
            seq: Seq::ZERO,
            state: QuoteState::Live,
        }
    }

    #[test]
    fn leg_rejects_zero_notional() {
        assert_eq!(
            Leg::new(ContractId::new("C").unwrap(), LegSide::BuyYes, Amount::ZERO),
            Err(ZeroNotional)
        );
    }

    #[test]
    fn request_rejects_empty_legs() {
        assert_eq!(
            RfqRequest::open(RequestId::new(), PartyId::new(), vec![], t(10), t(0)),
            Err(EmptyLegs)
        );
    }

    #[test]
    fn open_request_starts_clean() {
        let r = RfqRequest::open(
            RequestId::new(),
            PartyId::new(),
            vec![leg(LegSide::BuyYes, 1_000)],
            t(10),
            t(0),
        )
        .unwrap();
        assert_eq!(r.state, RequestState::Open);
        assert!(r.quotes.is_empty() && r.escrows.is_empty());
        assert!(r.package.is_none() && r.accept_deadline.is_none() && r.fail_reason.is_none());
    }

    #[test]
    fn escrow_roles_follow_leg_side() {
        let requester = PartyId::new();
        let maker = PartyId::new();
        let rid = RequestId::new();
        let price = Price::new(3_000).unwrap();

        let buy = leg(LegSide::BuyYes, 10_000);
        let e = Escrow::new(rid, &buy, price, requester, maker);
        assert_eq!((e.yes_buyer, e.yes_seller), (requester, maker));
        assert_eq!(e.yes_buyer_amount, Amount::new(3_000));
        assert_eq!(e.yes_seller_amount, Amount::new(7_000));
        assert_eq!(e.notional, Amount::new(10_000));
        assert_eq!((e.request_id, e.leg_id), (rid, buy.id));

        let sell = leg(LegSide::SellYes, 10_000);
        let e = Escrow::new(rid, &sell, price, requester, maker);
        assert_eq!((e.yes_buyer, e.yes_seller), (maker, requester));
        assert_eq!(e.yes_buyer_amount + e.yes_seller_amount, e.notional);
    }

    #[test]
    fn maker_lock_is_the_mm_side_of_escrow() {
        let requester = PartyId::new();
        let rid = RequestId::new();

        // BuyYes: MM is Yes-seller, locks (1 - p) * n.
        let buy = leg(LegSide::BuyYes, 10_000);
        let q = quote(&buy, 3_000);
        assert_eq!(q.maker_lock(&buy), Amount::new(7_000));
        let e = Escrow::new(rid, &buy, q.price, requester, q.maker);
        assert_eq!(q.maker_lock(&buy), e.yes_seller_amount);

        // SellYes: MM is Yes-buyer, locks p * n.
        let sell = leg(LegSide::SellYes, 10_000);
        let q = quote(&sell, 3_000);
        assert_eq!(q.maker_lock(&sell), Amount::new(3_000));
        let e = Escrow::new(rid, &sell, q.price, requester, q.maker);
        assert_eq!(q.maker_lock(&sell), e.yes_buyer_amount);

        // Odd notional: MM lock + requester lock still == notional.
        let odd = leg(LegSide::BuyYes, 7);
        let q = quote(&odd, 3_333);
        let requester_side = q.price.yes_buyer_lock(odd.notional);
        assert_eq!(q.maker_lock(&odd) + requester_side, odd.notional);
    }

    #[test]
    fn ledger_account_total() {
        let a = LedgerAccount {
            free: Amount::new(1),
            reserved: Amount::new(2),
            escrowed: Amount::new(3),
        };
        assert_eq!(a.total(), Amount::new(6));
    }
}
