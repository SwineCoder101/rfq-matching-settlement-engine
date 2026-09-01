use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use uuid::Uuid;

use super::error::{ErrorBody, ErrorResponse};
use crate::domain::PartyId;

pub const PARTY_HEADER: &str = "x-party-id";

/// Identity claimed via the `x-party-id` header. There is no authentication beyond this.
#[derive(Debug, Clone, Copy)]
pub struct Party(pub PartyId);

fn unauthorized(message: &str) -> ErrorResponse {
    (StatusCode::UNAUTHORIZED, Json(ErrorBody::new("missing_party", message)))
}

impl<S: Send + Sync> FromRequestParts<S> for Party {
    type Rejection = ErrorResponse;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(PARTY_HEADER)
            .ok_or_else(|| unauthorized("missing x-party-id header"))?;
        let text = raw.to_str().map_err(|_| unauthorized("x-party-id must be a UUID"))?;
        let id = Uuid::parse_str(text).map_err(|_| unauthorized("x-party-id must be a UUID"))?;
        Ok(Party(PartyId::from(id)))
    }
}
