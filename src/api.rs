//! HTTP boundary: router, `x-party-id` extractor, request bodies, and the error mapping.
//! Handlers parse, send one command, and render the reply. No ledger I/O happens here.

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{delete, get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    Amount, ContractDescription, ContractId, InvalidContractDescription, InvalidContractId,
    InvalidPrice, Leg, LegId, LegSide, OracleOutcome, PartyId, Price, Quote, QuoteId, RequestId,
    RfqRequest, ZeroNotional,
};
use crate::engine::{EngineError, EngineHandle};
use crate::ledger::LedgerAccount;

#[derive(Debug, Clone)]
pub struct AppState {
    pub engine: EngineHandle,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/ledger/credit", post(credit))
        .route("/v1/ledger/{party_id}", get(balance))
        .route("/v1/requests", post(create_request))
        .route("/v1/requests/{id}", get(get_request))
        .route("/v1/requests/{id}/quotes", post(submit_quote))
        .route("/v1/requests/{id}/accept", post(accept))
        .route("/v1/requests/{id}/reject", post(reject))
        .route("/v1/quotes/{id}", delete(cancel_quote))
        .route("/v1/oracle/resolve", post(resolve))
        .with_state(state)
}

// ---------------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------------

pub const PARTY_HEADER: &str = "x-party-id";

/// Identity claimed via the `x-party-id` header. There is no authentication beyond this.
#[derive(Debug, Clone, Copy)]
struct Party(PartyId);

impl<S: Send + Sync> FromRequestParts<S> for Party {
    type Rejection = ErrorResponse;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let missing = |message| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    code: "missing_party",
                    message,
                }),
            )
        };
        let raw = parts
            .headers
            .get(PARTY_HEADER)
            .ok_or_else(|| missing("missing x-party-id header".into()))?;
        let text = raw
            .to_str()
            .map_err(|_| missing("x-party-id must be a UUID".into()))?;
        let id = Uuid::parse_str(text).map_err(|_| missing("x-party-id must be a UUID".into()))?;
        Ok(Party(PartyId::from(id)))
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// JSON error envelope: `{ "code": "wrong_state", "message": "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

type ErrorResponse = (StatusCode, Json<ErrorBody>);

impl From<EngineError> for ErrorResponse {
    fn from(e: EngineError) -> Self {
        let (status, code) = match &e {
            EngineError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            EngineError::NotOwner => (StatusCode::FORBIDDEN, "not_owner"),
            EngineError::WrongState { .. } => (StatusCode::CONFLICT, "wrong_state"),
            EngineError::QuoteNotLive => (StatusCode::CONFLICT, "quote_not_live"),
            EngineError::InsufficientFunds { .. } => {
                (StatusCode::PAYMENT_REQUIRED, "insufficient_funds")
            }
            EngineError::QuoteExpired => (StatusCode::BAD_REQUEST, "quote_expired"),
            EngineError::QuoteTooSmall => (StatusCode::BAD_REQUEST, "quote_too_small"),
            EngineError::QuoteExpiresBeforeAcceptWindow => (
                StatusCode::BAD_REQUEST,
                "quote_expires_before_accept_window",
            ),
            EngineError::DeadlineInPast => (StatusCode::BAD_REQUEST, "deadline_in_past"),
            EngineError::EmptyLegs => (StatusCode::BAD_REQUEST, "empty_legs"),
            EngineError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "engine_unavailable"),
        };
        (
            status,
            Json(ErrorBody {
                code,
                message: e.to_string(),
            }),
        )
    }
}

/// Body validation failures raised while converting raw input into domain types. All `400`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum ApiError {
    #[error(transparent)]
    InvalidPrice(#[from] InvalidPrice),
    #[error(transparent)]
    InvalidContractId(#[from] InvalidContractId),
    #[error(transparent)]
    InvalidContractDescription(#[from] InvalidContractDescription),
    #[error(transparent)]
    ZeroNotional(#[from] ZeroNotional),
    #[error("amount must be greater than zero")]
    ZeroAmount,
    #[error("quote size must be greater than zero")]
    ZeroSize,
}

impl From<ApiError> for ErrorResponse {
    fn from(e: ApiError) -> Self {
        let code = match e {
            ApiError::InvalidPrice(_) => "invalid_price",
            ApiError::InvalidContractId(_) => "invalid_contract_id",
            ApiError::InvalidContractDescription(_) => "invalid_contract_description",
            ApiError::ZeroNotional(_) => "zero_notional",
            ApiError::ZeroAmount => "zero_amount",
            ApiError::ZeroSize => "zero_size",
        };
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                code,
                message: e.to_string(),
            }),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreditBody {
    party_id: Uuid,
    /// Minor units.
    amount: u64,
}

#[derive(Debug, Deserialize)]
struct LegBody {
    contract: String,
    description: String,
    side: LegSide,
    /// Minor units.
    notional: u64,
}

impl TryFrom<LegBody> for Leg {
    type Error = ApiError;

    fn try_from(b: LegBody) -> Result<Self, Self::Error> {
        let contract = ContractId::new(b.contract)?;
        let description = ContractDescription::new(b.description)?;
        Ok(Leg::new(
            contract,
            description,
            b.side,
            Amount::new(b.notional),
        )?)
    }
}

/// Requester comes from `x-party-id`, not the body.
#[derive(Debug, Deserialize)]
struct CreateRequestBody {
    legs: Vec<LegBody>,
    response_deadline: DateTime<Utc>,
}

/// Maker comes from `x-party-id`, request id from the path.
#[derive(Debug, Deserialize)]
struct SubmitQuoteBody {
    leg_id: Uuid,
    /// Basis points, `1..=9999`.
    price_bps: u32,
    /// Minor units.
    size: u64,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct ResolveBody {
    request_id: Uuid,
    outcome: OracleOutcome,
}

#[derive(Debug, Serialize)]
struct BalanceView {
    party_id: PartyId,
    #[serde(flatten)]
    account: LedgerAccount,
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

type ApiResult<T> = Result<T, ErrorResponse>;

async fn credit(
    State(app): State<AppState>,
    Json(body): Json<CreditBody>,
) -> ApiResult<Json<BalanceView>> {
    if body.amount == 0 {
        return Err(ApiError::ZeroAmount.into());
    }
    let party = PartyId::from(body.party_id);
    let account = app.engine.credit(party, Amount::new(body.amount)).await?;
    Ok(Json(BalanceView {
        party_id: party,
        account,
    }))
}

async fn balance(
    State(app): State<AppState>,
    Path(party_id): Path<Uuid>,
) -> ApiResult<Json<BalanceView>> {
    let party = PartyId::from(party_id);
    let account = app.engine.balance(party).await?;
    Ok(Json(BalanceView {
        party_id: party,
        account,
    }))
}

async fn create_request(
    State(app): State<AppState>,
    Party(requester): Party,
    Json(body): Json<CreateRequestBody>,
) -> ApiResult<(StatusCode, Json<RfqRequest>)> {
    let legs = body
        .legs
        .into_iter()
        .map(Leg::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let request = app
        .engine
        .submit_request(requester, legs, body.response_deadline)
        .await?;
    Ok((StatusCode::CREATED, Json(request)))
}

async fn get_request(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RfqRequest>> {
    Ok(Json(app.engine.get_request(RequestId::from(id)).await?))
}

async fn submit_quote(
    State(app): State<AppState>,
    Party(maker): Party,
    Path(id): Path<Uuid>,
    Json(body): Json<SubmitQuoteBody>,
) -> ApiResult<(StatusCode, Json<Quote>)> {
    if body.size == 0 {
        return Err(ApiError::ZeroSize.into());
    }
    let price = Price::new(body.price_bps).map_err(ApiError::from)?;
    let quote = app
        .engine
        .submit_quote(
            maker,
            RequestId::from(id),
            LegId::from(body.leg_id),
            price,
            Amount::new(body.size),
            body.expires_at,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(quote)))
}

async fn cancel_quote(
    State(app): State<AppState>,
    Party(maker): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    app.engine.cancel_quote(maker, QuoteId::from(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn accept(
    State(app): State<AppState>,
    Party(requester): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RfqRequest>> {
    Ok(Json(
        app.engine.accept(requester, RequestId::from(id)).await?,
    ))
}

async fn reject(
    State(app): State<AppState>,
    Party(requester): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RfqRequest>> {
    Ok(Json(
        app.engine.reject(requester, RequestId::from(id)).await?,
    ))
}

async fn resolve(
    State(app): State<AppState>,
    Json(body): Json<ResolveBody>,
) -> ApiResult<Json<RfqRequest>> {
    Ok(Json(
        app.engine
            .resolve(RequestId::from(body.request_id), body.outcome)
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RequestState;

    fn status(e: EngineError) -> StatusCode {
        let (s, _): ErrorResponse = e.into();
        s
    }

    #[test]
    fn engine_errors_map_to_expected_statuses() {
        assert_eq!(status(EngineError::NotFound), StatusCode::NOT_FOUND);
        assert_eq!(status(EngineError::NotOwner), StatusCode::FORBIDDEN);
        assert_eq!(
            status(EngineError::WrongState {
                expected: RequestState::Presented,
                actual: RequestState::Settled
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(status(EngineError::QuoteNotLive), StatusCode::CONFLICT);
        assert_eq!(
            status(EngineError::Unavailable),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(EngineError::InsufficientFunds {
                party: PartyId::new(),
                needed: Amount::new(2),
                available: Amount::new(1)
            }),
            StatusCode::PAYMENT_REQUIRED
        );
        for e in [
            EngineError::QuoteExpired,
            EngineError::QuoteTooSmall,
            EngineError::QuoteExpiresBeforeAcceptWindow,
            EngineError::DeadlineInPast,
            EngineError::EmptyLegs,
        ] {
            assert_eq!(status(e.clone()), StatusCode::BAD_REQUEST, "{e:?}");
        }
    }

    #[test]
    fn error_body_carries_code_and_message() {
        let (_, Json(body)): ErrorResponse = EngineError::NotOwner.into();
        assert_eq!(body.code, "not_owner");
        assert!(!body.message.is_empty());
        assert_eq!(serde_json::to_value(&body).unwrap()["code"], "not_owner");
    }

    #[test]
    fn zero_amount_and_zero_size_are_bad_request() {
        for e in [ApiError::ZeroAmount, ApiError::ZeroSize] {
            let (s, Json(body)): ErrorResponse = e.clone().into();
            assert_eq!(s, StatusCode::BAD_REQUEST, "{e:?}");
            assert!(body.code.starts_with("zero_"));
        }
    }

    #[test]
    fn api_errors_are_bad_request() {
        let (s, Json(body)): ErrorResponse = ApiError::from(InvalidPrice { bps: 0 }).into();
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body.code, "invalid_price");
    }

    #[test]
    fn leg_body_parses_into_domain_leg() {
        let body: LegBody = serde_json::from_str(
            r#"{"contract":"BTC-100K","description":"Settles Yes if BTC/USD on Coinbase is above 100000.00 at 2026-12-31T00:00:00Z; otherwise No.","side":"buy_yes","notional":1000}"#,
        )
        .unwrap();
        let leg = Leg::try_from(body).unwrap();
        assert_eq!(leg.contract.as_str(), "BTC-100K");
        assert_eq!(
            leg.description.as_str(),
            "Settles Yes if BTC/USD on Coinbase is above 100000.00 at 2026-12-31T00:00:00Z; otherwise No."
        );
        assert_eq!(leg.side, LegSide::BuyYes);
        assert_eq!(leg.notional, Amount::new(1_000));
    }

    #[test]
    fn leg_body_rejects_bad_input() {
        let leg = |contract: &str, description: &str, notional: u64| LegBody {
            contract: contract.into(),
            description: description.into(),
            side: LegSide::SellYes,
            notional,
        };
        assert!(matches!(
            Leg::try_from(leg("C", "d", 0)),
            Err(ApiError::ZeroNotional(_))
        ));
        assert!(matches!(
            Leg::try_from(leg(" ", "d", 1)),
            Err(ApiError::InvalidContractId(_))
        ));
        assert!(matches!(
            Leg::try_from(leg("C", "  ", 1)),
            Err(ApiError::InvalidContractDescription(_))
        ));
        let body: Result<LegBody, _> =
            serde_json::from_str(r#"{"contract":"C","side":"buy_yes","notional":1}"#);
        assert!(body.is_err(), "description is required on the wire");
    }

    #[test]
    fn resolve_body_parses_outcome() {
        let body: ResolveBody = serde_json::from_str(&format!(
            r#"{{"request_id":"{}","outcome":"invalid"}}"#,
            Uuid::nil()
        ))
        .unwrap();
        assert_eq!(
            RequestId::from(body.request_id),
            RequestId::from(Uuid::nil())
        );
        assert_eq!(body.outcome, OracleOutcome::Invalid);
    }

    #[test]
    fn balance_view_flattens_account() {
        let party = PartyId::new();
        let view = BalanceView {
            party_id: party,
            account: LedgerAccount {
                free: Amount::new(1),
                reserved: Amount::new(2),
                escrowed: Amount::new(3),
            },
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["party_id"], party.to_string());
        assert_eq!(
            (
                json["free"].as_u64(),
                json["reserved"].as_u64(),
                json["escrowed"].as_u64()
            ),
            (Some(1), Some(2), Some(3))
        );
    }
}
