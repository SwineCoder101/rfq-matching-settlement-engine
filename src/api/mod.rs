//! HTTP boundary: serde DTOs and the `EngineError` → HTTP status mapping.
//! Handlers are not implemented yet; this module only defines what they parse and return.

pub mod dto;
pub mod error;

pub use dto::{
    BalanceView, CreateRequestBody, CreditBody, EscrowView, LegBody, LegSideDto, LegView, OutcomeDto,
    PackageView, QuoteView, RequestView, ResolveBody, SelectionView, SubmitQuoteBody,
};
pub use error::{ApiError, ErrorBody, ErrorResponse};
