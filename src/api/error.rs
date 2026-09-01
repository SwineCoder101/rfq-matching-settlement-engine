use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;

use crate::domain::{EmptyLegs, EngineError, InvalidContractId, InvalidPrice, ZeroNotional};

/// JSON error envelope: `{ "code": "wrong_state", "message": "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// What a handler returns on failure. `(StatusCode, Json<T>)` already implements
/// `axum::response::IntoResponse`.
pub type ErrorResponse = (StatusCode, Json<ErrorBody>);

impl From<EngineError> for (StatusCode, Json<ErrorBody>) {
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
            EngineError::QuoteExpiresBeforeAcceptWindow => {
                (StatusCode::BAD_REQUEST, "quote_expires_before_accept_window")
            }
            EngineError::InvalidPrice => (StatusCode::BAD_REQUEST, "invalid_price"),
            EngineError::DeadlineInPast => (StatusCode::BAD_REQUEST, "deadline_in_past"),
            EngineError::EmptyLegs => (StatusCode::BAD_REQUEST, "empty_legs"),
            EngineError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "engine_unavailable"),
        };
        (status, Json(ErrorBody { code, message: e.to_string() }))
    }
}

/// Request-body validation failures raised while converting DTOs into domain types.
/// All map to `400 Bad Request`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    InvalidPrice(#[from] InvalidPrice),
    #[error(transparent)]
    InvalidContractId(#[from] InvalidContractId),
    #[error(transparent)]
    ZeroNotional(#[from] ZeroNotional),
    #[error(transparent)]
    EmptyLegs(#[from] EmptyLegs),
    #[error("amount must be greater than zero")]
    ZeroAmount,
    #[error("quote size must be greater than zero")]
    ZeroSize,
}

impl ApiError {
    fn code(&self) -> &'static str {
        match self {
            ApiError::InvalidPrice(_) => "invalid_price",
            ApiError::InvalidContractId(_) => "invalid_contract_id",
            ApiError::ZeroNotional(_) => "zero_notional",
            ApiError::EmptyLegs(_) => "empty_legs",
            ApiError::ZeroAmount => "zero_amount",
            ApiError::ZeroSize => "zero_size",
        }
    }
}

impl From<ApiError> for (StatusCode, Json<ErrorBody>) {
    fn from(e: ApiError) -> Self {
        (StatusCode::BAD_REQUEST, Json(ErrorBody { code: e.code(), message: e.to_string() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Amount, PartyId, RequestState};

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
        assert_eq!(status(EngineError::Unavailable), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            status(EngineError::InsufficientFunds {
                party: PartyId::new(),
                needed: Amount::new(2),
                available: Amount::new(1),
            }),
            StatusCode::PAYMENT_REQUIRED
        );
        for e in [
            EngineError::QuoteExpired,
            EngineError::QuoteTooSmall,
            EngineError::QuoteExpiresBeforeAcceptWindow,
            EngineError::InvalidPrice,
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
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["code"], "not_owner");
    }

    #[test]
    fn api_errors_are_bad_request() {
        let (s, Json(body)): ErrorResponse = ApiError::from(InvalidPrice { bps: 0 }).into();
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body.code, "invalid_price");
    }
}
