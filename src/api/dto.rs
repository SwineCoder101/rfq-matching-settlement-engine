//! Wire types. Request bodies derive `Deserialize` and convert into domain types via `TryFrom`;
//! response views derive `Serialize` and are built from domain aggregates with `From`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::ApiError;
use crate::domain::{
    Amount, ContractId, Escrow, FailReason, LedgerAccount, Leg, LegSide, OracleOutcome, Package,
    Price, Quote, QuoteState, RequestState, RfqRequest,
};
use crate::domain::{LegId, PartyId, QuoteId, RequestId};

// ---------------------------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------------------------

/// Wire form of [`LegSide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegSideDto {
    BuyYes,
    SellYes,
}

impl From<LegSideDto> for LegSide {
    fn from(s: LegSideDto) -> Self {
        match s {
            LegSideDto::BuyYes => LegSide::BuyYes,
            LegSideDto::SellYes => LegSide::SellYes,
        }
    }
}

/// Wire form of [`OracleOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeDto {
    Yes,
    No,
    Invalid,
    Disputed,
}

impl From<OutcomeDto> for OracleOutcome {
    fn from(o: OutcomeDto) -> Self {
        match o {
            OutcomeDto::Yes => OracleOutcome::Yes,
            OutcomeDto::No => OracleOutcome::No,
            OutcomeDto::Invalid => OracleOutcome::Invalid,
            OutcomeDto::Disputed => OracleOutcome::Disputed,
        }
    }
}

/// `POST /v1/ledger/credit`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreditBody {
    pub party_id: Uuid,
    /// Minor units.
    pub amount: u64,
}

impl TryFrom<CreditBody> for (PartyId, Amount) {
    type Error = ApiError;

    fn try_from(b: CreditBody) -> Result<Self, Self::Error> {
        if b.amount == 0 {
            return Err(ApiError::ZeroAmount);
        }
        Ok((PartyId::from(b.party_id), Amount::new(b.amount)))
    }
}

/// One leg inside `POST /v1/requests`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LegBody {
    pub contract: String,
    pub side: LegSideDto,
    /// Minor units.
    pub notional: u64,
}

impl TryFrom<LegBody> for Leg {
    type Error = ApiError;

    fn try_from(b: LegBody) -> Result<Self, Self::Error> {
        let contract = ContractId::try_from(b.contract)?;
        Ok(Leg::new(contract, b.side.into(), Amount::new(b.notional))?)
    }
}

/// `POST /v1/requests`. Requester comes from `x-party-id`, not the body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateRequestBody {
    pub legs: Vec<LegBody>,
    pub response_deadline: DateTime<Utc>,
}

impl TryFrom<CreateRequestBody> for (Vec<Leg>, DateTime<Utc>) {
    type Error = ApiError;

    fn try_from(b: CreateRequestBody) -> Result<Self, Self::Error> {
        if b.legs.is_empty() {
            return Err(crate::domain::EmptyLegs.into());
        }
        let legs = b.legs.into_iter().map(Leg::try_from).collect::<Result<Vec<_>, _>>()?;
        Ok((legs, b.response_deadline))
    }
}

/// `POST /v1/requests/{id}/quotes`. Maker comes from `x-party-id`, request id from the path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SubmitQuoteBody {
    pub leg_id: Uuid,
    /// Basis points, `1..=9999`.
    pub price_bps: u32,
    /// Minor units.
    pub size: u64,
    pub expires_at: DateTime<Utc>,
}

impl TryFrom<SubmitQuoteBody> for (LegId, Price, Amount, DateTime<Utc>) {
    type Error = ApiError;

    fn try_from(b: SubmitQuoteBody) -> Result<Self, Self::Error> {
        if b.size == 0 {
            return Err(ApiError::ZeroSize);
        }
        let price = Price::try_from(b.price_bps)?;
        Ok((LegId::from(b.leg_id), price, Amount::new(b.size), b.expires_at))
    }
}

/// `POST /v1/oracle/resolve`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResolveBody {
    pub request_id: Uuid,
    pub outcome: OutcomeDto,
}

impl From<ResolveBody> for (RequestId, OracleOutcome) {
    fn from(b: ResolveBody) -> Self {
        (RequestId::from(b.request_id), b.outcome.into())
    }
}

// ---------------------------------------------------------------------------------------------
// Response views
// ---------------------------------------------------------------------------------------------

/// `GET /v1/ledger/{party_id}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BalanceView {
    pub party_id: PartyId,
    pub free: Amount,
    pub reserved: Amount,
    pub escrowed: Amount,
}

impl From<(PartyId, LedgerAccount)> for BalanceView {
    fn from((party_id, a): (PartyId, LedgerAccount)) -> Self {
        Self { party_id, free: a.free, reserved: a.reserved, escrowed: a.escrowed }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegView {
    pub id: LegId,
    pub contract: ContractId,
    pub side: LegSide,
    pub notional: Amount,
}

impl From<&Leg> for LegView {
    fn from(l: &Leg) -> Self {
        Self { id: l.id, contract: l.contract.clone(), side: l.side, notional: l.notional }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuoteView {
    pub id: QuoteId,
    pub leg_id: LegId,
    pub maker: PartyId,
    pub price_bps: Price,
    pub size: Amount,
    pub expires_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    pub state: QuoteState,
}

impl From<&Quote> for QuoteView {
    fn from(q: &Quote) -> Self {
        Self {
            id: q.id,
            leg_id: q.leg_id,
            maker: q.maker,
            price_bps: q.price,
            size: q.size,
            expires_at: q.expires_at,
            submitted_at: q.submitted_at,
            state: q.state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectionView {
    pub leg_id: LegId,
    pub quote_id: QuoteId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageView {
    pub selections: Vec<SelectionView>,
}

impl From<&Package> for PackageView {
    fn from(p: &Package) -> Self {
        Self {
            selections: p
                .selections
                .iter()
                .map(|s| SelectionView { leg_id: s.leg_id, quote_id: s.quote_id })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscrowView {
    pub leg_id: LegId,
    pub yes_buyer: PartyId,
    pub yes_seller: PartyId,
    pub yes_buyer_amount: Amount,
    pub yes_seller_amount: Amount,
    pub notional: Amount,
}

impl From<&Escrow> for EscrowView {
    fn from(e: &Escrow) -> Self {
        Self {
            leg_id: e.leg_id,
            yes_buyer: e.yes_buyer,
            yes_seller: e.yes_seller,
            yes_buyer_amount: e.yes_buyer_amount,
            yes_seller_amount: e.yes_seller_amount,
            notional: e.notional,
        }
    }
}

/// `GET /v1/requests/{id}` — state, legs, quotes, package if presented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestView {
    pub id: RequestId,
    pub requester: PartyId,
    pub state: RequestState,
    pub legs: Vec<LegView>,
    pub quotes: Vec<QuoteView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageView>,
    pub escrows: Vec<EscrowView>,
    pub response_deadline: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_deadline: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<FailReason>,
    pub created_at: DateTime<Utc>,
}

impl From<&RfqRequest> for RequestView {
    fn from(r: &RfqRequest) -> Self {
        Self {
            id: r.id,
            requester: r.requester,
            state: r.state,
            legs: r.legs.iter().map(LegView::from).collect(),
            quotes: r.quotes.iter().map(QuoteView::from).collect(),
            package: r.package.as_ref().map(PackageView::from),
            escrows: r.escrows.iter().map(EscrowView::from).collect(),
            response_deadline: r.response_deadline,
            accept_deadline: r.accept_deadline,
            fail_reason: r.fail_reason,
            created_at: r.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn leg_body_parses_into_domain_leg() {
        let body: LegBody =
            serde_json::from_str(r#"{"contract":"BTC-100K","side":"buy_yes","notional":1000}"#)
                .unwrap();
        let leg = Leg::try_from(body).unwrap();
        assert_eq!(leg.contract.as_str(), "BTC-100K");
        assert_eq!(leg.side, LegSide::BuyYes);
        assert_eq!(leg.notional, Amount::new(1_000));
    }

    #[test]
    fn leg_body_rejects_bad_input() {
        let zero = LegBody { contract: "C".into(), side: LegSideDto::SellYes, notional: 0 };
        assert!(matches!(Leg::try_from(zero), Err(ApiError::ZeroNotional(_))));
        let blank = LegBody { contract: " ".into(), side: LegSideDto::SellYes, notional: 1 };
        assert!(matches!(Leg::try_from(blank), Err(ApiError::InvalidContractId(_))));
    }

    #[test]
    fn create_request_body_requires_legs() {
        let body = CreateRequestBody { legs: vec![], response_deadline: t(10) };
        let r: Result<(Vec<Leg>, DateTime<Utc>), _> = body.try_into();
        assert!(matches!(r, Err(ApiError::EmptyLegs(_))));
    }

    #[test]
    fn quote_body_validates_price_and_size() {
        let leg_id = Uuid::new_v4();
        let ok = SubmitQuoteBody { leg_id, price_bps: 2_500, size: 10, expires_at: t(10) };
        let (lid, price, size, exp): (LegId, Price, Amount, DateTime<Utc>) = ok.try_into().unwrap();
        assert_eq!(lid, LegId::from(leg_id));
        assert_eq!(price.bps(), 2_500);
        assert_eq!(size, Amount::new(10));
        assert_eq!(exp, t(10));

        let bad_price = SubmitQuoteBody { leg_id, price_bps: 10_000, size: 10, expires_at: t(10) };
        let r: Result<(LegId, Price, Amount, DateTime<Utc>), _> = bad_price.try_into();
        assert!(matches!(r, Err(ApiError::InvalidPrice(_))));

        let zero_size = SubmitQuoteBody { leg_id, price_bps: 100, size: 0, expires_at: t(10) };
        let r: Result<(LegId, Price, Amount, DateTime<Utc>), _> = zero_size.try_into();
        assert_eq!(r, Err(ApiError::ZeroSize));
    }

    #[test]
    fn credit_body_rejects_zero() {
        let body = CreditBody { party_id: Uuid::new_v4(), amount: 0 };
        let r: Result<(PartyId, Amount), _> = body.try_into();
        assert_eq!(r, Err(ApiError::ZeroAmount));
    }

    #[test]
    fn resolve_body_maps_outcome() {
        let body: ResolveBody = serde_json::from_str(&format!(
            r#"{{"request_id":"{}","outcome":"invalid"}}"#,
            Uuid::nil()
        ))
        .unwrap();
        let (rid, outcome): (RequestId, OracleOutcome) = body.into();
        assert_eq!(rid, RequestId::from(Uuid::nil()));
        assert_eq!(outcome, OracleOutcome::Invalid);
    }

    #[test]
    fn request_view_serializes_domain_aggregate() {
        let leg = Leg::new(ContractId::new("C").unwrap(), LegSide::BuyYes, Amount::new(500)).unwrap();
        let req = RfqRequest::open(RequestId::new(), PartyId::new(), vec![leg], t(10), t(0)).unwrap();
        let json = serde_json::to_value(RequestView::from(&req)).unwrap();
        assert_eq!(json["state"], "open");
        assert_eq!(json["legs"][0]["side"], "buy_yes");
        assert_eq!(json["legs"][0]["notional"], 500);
        assert!(json.get("package").is_none(), "absent package is omitted");
        assert!(json.get("fail_reason").is_none());
    }
}
