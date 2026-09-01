//! Axum handlers. Each one parses JSON, sends a command, and maps the reply to HTTP.
//! No ledger I/O happens here.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::dto::{
    BalanceView, CreateRequestBody, CreditBody, QuoteView, RequestView, ResolveBody,
    SubmitQuoteBody,
};
use super::error::ErrorResponse;
use super::extract::Party;
use crate::domain::{Amount, Leg, LegId, OracleOutcome, PartyId, Price, QuoteId, RequestId};
use crate::engine::EngineHandle;

#[derive(Debug, Clone)]
pub struct AppState {
    pub engine: EngineHandle,
}

type ApiResult<T> = Result<T, ErrorResponse>;

pub async fn credit(
    State(app): State<AppState>,
    Json(body): Json<CreditBody>,
) -> ApiResult<Json<BalanceView>> {
    let (party, amount): (PartyId, Amount) = body.try_into()?;
    let account = app.engine.credit(party, amount).await?;
    Ok(Json(BalanceView::from((party, account))))
}

pub async fn balance(
    State(app): State<AppState>,
    Path(party_id): Path<Uuid>,
) -> ApiResult<Json<BalanceView>> {
    let party = PartyId::from(party_id);
    let account = app.engine.balance(party).await?;
    Ok(Json(BalanceView::from((party, account))))
}

pub async fn create_request(
    State(app): State<AppState>,
    Party(requester): Party,
    Json(body): Json<CreateRequestBody>,
) -> ApiResult<(StatusCode, Json<RequestView>)> {
    let (legs, response_deadline): (Vec<Leg>, DateTime<Utc>) = body.try_into()?;
    let request = app.engine.submit_request(requester, legs, response_deadline).await?;
    Ok((StatusCode::CREATED, Json(RequestView::from(&request))))
}

pub async fn get_request(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RequestView>> {
    let request = app.engine.get_request(RequestId::from(id)).await?;
    Ok(Json(RequestView::from(&request)))
}

pub async fn submit_quote(
    State(app): State<AppState>,
    Party(maker): Party,
    Path(id): Path<Uuid>,
    Json(body): Json<SubmitQuoteBody>,
) -> ApiResult<(StatusCode, Json<QuoteView>)> {
    let (leg_id, price, size, expires_at): (LegId, Price, Amount, DateTime<Utc>) = body.try_into()?;
    let quote = app
        .engine
        .submit_quote(maker, RequestId::from(id), leg_id, price, size, expires_at)
        .await?;
    Ok((StatusCode::CREATED, Json(QuoteView::from(&quote))))
}

pub async fn cancel_quote(
    State(app): State<AppState>,
    Party(maker): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    app.engine.cancel_quote(maker, QuoteId::from(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn accept(
    State(app): State<AppState>,
    Party(requester): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RequestView>> {
    let request = app.engine.accept(requester, RequestId::from(id)).await?;
    Ok(Json(RequestView::from(&request)))
}

pub async fn reject(
    State(app): State<AppState>,
    Party(requester): Party,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RequestView>> {
    let request = app.engine.reject(requester, RequestId::from(id)).await?;
    Ok(Json(RequestView::from(&request)))
}

pub async fn resolve(
    State(app): State<AppState>,
    Json(body): Json<ResolveBody>,
) -> ApiResult<Json<RequestView>> {
    let (request_id, outcome): (RequestId, OracleOutcome) = body.into();
    let request = app.engine.resolve(request_id, outcome).await?;
    Ok(Json(RequestView::from(&request)))
}
