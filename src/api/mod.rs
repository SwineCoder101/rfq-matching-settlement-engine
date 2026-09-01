//! HTTP boundary: serde DTOs, the `EngineError` → HTTP mapping, the `x-party-id` extractor,
//! and the Axum router. Handlers never touch the ledger; they send commands to the engine.

pub mod dto;
pub mod error;
pub mod extract;
pub mod handlers;

use axum::Router;
use axum::routing::{get, post};

pub use dto::{
    BalanceView, CreateRequestBody, CreditBody, EscrowView, LegBody, LegSideDto, LegView,
    OutcomeDto, PackageView, QuoteView, RequestView, ResolveBody, SelectionView, SubmitQuoteBody,
};
pub use error::{ApiError, ErrorBody, ErrorResponse};
pub use extract::{PARTY_HEADER, Party};
pub use handlers::AppState;

/// The full `/v1` surface from ARCHITECTURE.md.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/ledger/credit", post(handlers::credit))
        .route("/v1/ledger/{party_id}", get(handlers::balance))
        .route("/v1/requests", post(handlers::create_request))
        .route("/v1/requests/{id}", get(handlers::get_request))
        .route("/v1/requests/{id}/quotes", post(handlers::submit_quote))
        .route("/v1/requests/{id}/accept", post(handlers::accept))
        .route("/v1/requests/{id}/reject", post(handlers::reject))
        .route("/v1/quotes/{id}", axum::routing::delete(handlers::cancel_quote))
        .route("/v1/oracle/resolve", post(handlers::resolve))
        .with_state(state)
}
