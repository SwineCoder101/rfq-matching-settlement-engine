//! Enums from the state diagrams in `docs/ARCHITECTURE.md`. Variant sets are exact.

use serde::Serialize;

use super::ids::LegId;

/// Which side of the binary contract the *requester* wants on a leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegSide {
    BuyYes,
    SellYes,
}

impl LegSide {
    /// `BuyYes` → the requester is the Yes-buyer; `SellYes` → the maker is the Yes-buyer.
    pub const fn requester_buys_yes(self) -> bool {
        matches!(self, LegSide::BuyYes)
    }
}

/// Request state machine (see "Request state machine" in ARCHITECTURE.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Open,
    Presented,
    Locked,
    Disputed,
    Settled,
    Unwound,
    Failed,
}

impl RequestState {
    /// `Settled`, `Unwound`, and `Failed` are terminal; a second accept/reject is a 409.
    pub const fn is_terminal(self) -> bool {
        matches!(self, RequestState::Settled | RequestState::Unwound | RequestState::Failed)
    }
}

/// Quote lifecycle (see "Quote lifecycle" in ARCHITECTURE.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteState {
    Live,
    Selected,
    Locked,
    Released,
}

/// What the oracle reports for a contract. "Unavailable / delayed" is `None` from
/// [`crate::domain::Oracle::outcome`], not a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleOutcome {
    Yes,
    No,
    Invalid,
    Disputed,
}

/// Why a request ended in `RequestState::Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "reason", content = "leg_id", rename_all = "snake_case")]
pub enum FailReason {
    /// At the response deadline this leg had no eligible live quote.
    LegUnmatched(LegId),
    /// The requester rejected the presented package.
    Rejected,
    /// The requester neither accepted nor rejected before `accept_deadline`.
    AcceptWindowExpired,
    /// `lock_batch` failed because the requester's free balance could not cover their side.
    InsufficientRequesterFunds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states() {
        assert!(RequestState::Settled.is_terminal());
        assert!(RequestState::Unwound.is_terminal());
        assert!(RequestState::Failed.is_terminal());
        assert!(!RequestState::Open.is_terminal());
        assert!(!RequestState::Presented.is_terminal());
        assert!(!RequestState::Locked.is_terminal());
        assert!(!RequestState::Disputed.is_terminal());
    }

    #[test]
    fn leg_side_role() {
        assert!(LegSide::BuyYes.requester_buys_yes());
        assert!(!LegSide::SellYes.requester_buys_yes());
    }

    #[test]
    fn fail_reason_serializes_with_tag() {
        let leg = LegId::new();
        let json = serde_json::to_value(FailReason::LegUnmatched(leg)).unwrap();
        assert_eq!(json["reason"], "leg_unmatched");
        assert_eq!(json["leg_id"], leg.to_string());
        let json = serde_json::to_value(FailReason::Rejected).unwrap();
        assert_eq!(json["reason"], "rejected");
    }
}
