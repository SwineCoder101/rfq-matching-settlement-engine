//! Aggregates: `RfqRequest` (root), `Leg`, `Quote`, `Package`, `Escrow`. These serialize
//! directly as the API's response bodies.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::ids::{ContractDescription, ContractId, LegId, PartyId, QuoteId, RequestId, Seq};
use super::money::{Amount, Price};
use super::state::{FailReason, LegSide, QuoteState, RequestState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("leg notional must be greater than zero")]
pub struct ZeroNotional;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a request must have at least one leg")]
pub struct EmptyLegs;

/// One binary contract, the requester's side, and a notional. Not an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Leg {
    pub id: LegId,
    pub contract: ContractId,
    pub description: ContractDescription,
    pub side: LegSide,
    pub notional: Amount,
}

impl Leg {
    pub fn new(
        contract: ContractId,
        description: ContractDescription,
        side: LegSide,
        notional: Amount,
    ) -> Result<Self, ZeroNotional> {
        if notional.is_zero() {
            return Err(ZeroNotional);
        }
        Ok(Self {
            id: LegId::new(),
            contract,
            description,
            side,
            notional,
        })
    }
}

/// A market maker's firm quote on one leg. Holds a collateral reservation while `Live` or
/// `Selected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Quote {
    pub id: QuoteId,
    pub leg_id: LegId,
    pub maker: PartyId,
    #[serde(rename = "price_bps")]
    pub price: Price,
    pub size: Amount,
    pub expires_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    /// Engine-assigned submit order; tie-breaker in matching. Not part of the wire format.
    #[serde(skip)]
    pub seq: Seq,
    pub state: QuoteState,
}

impl Quote {
    /// What the maker reserves at submit: its side of the escrow at this price for the leg's
    /// full notional. The maker takes the side opposite the requester.
    pub fn maker_lock(&self, leg: &Leg) -> Amount {
        debug_assert_eq!(
            self.leg_id, leg.id,
            "maker_lock called with a quote from another leg"
        );
        if leg.side.requester_buys_yes() {
            self.price.yes_seller_lock(leg.notional)
        } else {
            self.price.yes_buyer_lock(leg.notional)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub leg_id: LegId,
    pub quote_id: QuoteId,
}

/// One selection per leg, shown to the requester once the request is `Presented`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub selections: Vec<Selection>,
}

/// Funds locked for one leg after accept. Yes-buyer locks `p * n`, Yes-seller `(1 - p) * n`,
/// total `n`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Escrow {
    pub leg_id: LegId,
    pub yes_buyer: PartyId,
    pub yes_seller: PartyId,
    pub yes_buyer_amount: Amount,
    pub yes_seller_amount: Amount,
    pub notional: Amount,
}

impl Escrow {
    pub fn new(leg: &Leg, price: Price, requester: PartyId, maker: PartyId) -> Self {
        let (yes_buyer, yes_seller) = if leg.side.requester_buys_yes() {
            (requester, maker)
        } else {
            (maker, requester)
        };
        Self {
            leg_id: leg.id,
            yes_buyer,
            yes_seller,
            yes_buyer_amount: price.yes_buyer_lock(leg.notional),
            yes_seller_amount: price.yes_seller_lock(leg.notional),
            notional: leg.notional,
        }
    }
}

/// Aggregate root. Owns legs, quotes, deadlines, package, escrows, and `RequestState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RfqRequest {
    pub id: RequestId,
    pub requester: PartyId,
    pub legs: Vec<Leg>,
    pub quotes: Vec<Quote>,
    /// Absolute. At this instant the worker either presents a package or fails the request.
    pub response_deadline: DateTime<Utc>,
    /// Set when the request becomes `Presented`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_deadline: Option<DateTime<Utc>>,
    pub state: RequestState,
    /// Set when the request becomes `Presented`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
    /// Non-empty only from `Locked` onward.
    pub escrows: Vec<Escrow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<FailReason>,
    pub created_at: DateTime<Utc>,
}

impl RfqRequest {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn leg(side: LegSide, notional: u64) -> Leg {
        Leg::new(
            ContractId::new("C").unwrap(),
            ContractDescription::new("C resolves Yes").unwrap(),
            side,
            Amount::new(notional),
        )
        .unwrap()
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
            Leg::new(
                ContractId::new("C").unwrap(),
                ContractDescription::new("C").unwrap(),
                LegSide::BuyYes,
                Amount::ZERO
            ),
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
        let price = Price::new(3_000).unwrap();

        let buy = leg(LegSide::BuyYes, 10_000);
        let e = Escrow::new(&buy, price, requester, maker);
        assert_eq!((e.yes_buyer, e.yes_seller), (requester, maker));
        assert_eq!(e.yes_buyer_amount, Amount::new(3_000));
        assert_eq!(e.yes_seller_amount, Amount::new(7_000));
        assert_eq!(e.notional, Amount::new(10_000));
        assert_eq!(e.leg_id, buy.id);

        let sell = leg(LegSide::SellYes, 10_000);
        let e = Escrow::new(&sell, price, requester, maker);
        assert_eq!((e.yes_buyer, e.yes_seller), (maker, requester));
        assert_eq!(e.yes_buyer_amount + e.yes_seller_amount, e.notional);

        // Buying No is selling Yes; selling No is buying Yes. Same escrow either way.
        let buy_no = Escrow::new(&leg(LegSide::BuyNo, 10_000), price, requester, maker);
        assert_eq!((buy_no.yes_buyer, buy_no.yes_seller), (maker, requester));
        assert_eq!(
            buy_no.yes_seller_amount,
            Amount::new(7_000),
            "requester locks (1 - p) * n"
        );
        let sell_no = Escrow::new(&leg(LegSide::SellNo, 10_000), price, requester, maker);
        assert_eq!((sell_no.yes_buyer, sell_no.yes_seller), (requester, maker));
        assert_eq!(
            sell_no.yes_buyer_amount,
            Amount::new(3_000),
            "requester locks p * n"
        );
    }

    #[test]
    fn maker_lock_is_the_mm_side_of_escrow() {
        let requester = PartyId::new();

        // BuyYes: MM is Yes-seller, locks (1 - p) * n.
        let buy = leg(LegSide::BuyYes, 10_000);
        let q = quote(&buy, 3_000);
        assert_eq!(q.maker_lock(&buy), Amount::new(7_000));
        assert_eq!(
            q.maker_lock(&buy),
            Escrow::new(&buy, q.price, requester, q.maker).yes_seller_amount
        );

        // SellYes: MM is Yes-buyer, locks p * n.
        let sell = leg(LegSide::SellYes, 10_000);
        let q = quote(&sell, 3_000);
        assert_eq!(q.maker_lock(&sell), Amount::new(3_000));
        assert_eq!(
            q.maker_lock(&sell),
            Escrow::new(&sell, q.price, requester, q.maker).yes_buyer_amount
        );

        // BuyNo: MM sells No == buys Yes, locks p * n. SellNo: MM buys No == sells Yes.
        let buy_no = leg(LegSide::BuyNo, 10_000);
        assert_eq!(
            quote(&buy_no, 3_000).maker_lock(&buy_no),
            Amount::new(3_000)
        );
        let sell_no = leg(LegSide::SellNo, 10_000);
        assert_eq!(
            quote(&sell_no, 3_000).maker_lock(&sell_no),
            Amount::new(7_000)
        );

        // Odd notional: MM lock + requester lock still == notional.
        let odd = leg(LegSide::BuyYes, 7);
        let q = quote(&odd, 3_333);
        assert_eq!(
            q.maker_lock(&odd) + q.price.yes_buyer_lock(odd.notional),
            odd.notional
        );
    }

    #[test]
    fn request_serializes_as_the_wire_shape() {
        let req = RfqRequest::open(
            RequestId::new(),
            PartyId::new(),
            vec![leg(LegSide::BuyYes, 500)],
            t(10),
            t(0),
        )
        .unwrap();
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["state"], "open");
        assert_eq!(json["legs"][0]["side"], "buy_yes");
        assert_eq!(json["legs"][0]["description"], "C resolves Yes");
        assert_eq!(json["legs"][0]["notional"], 500);
        assert!(json.get("package").is_none(), "absent package is omitted");
        assert!(json.get("accept_deadline").is_none());
        assert!(json.get("fail_reason").is_none());

        let q = quote(&req.legs[0], 2_500);
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["price_bps"], 2_500);
        assert!(json.get("seq").is_none(), "seq is internal");
    }
}
