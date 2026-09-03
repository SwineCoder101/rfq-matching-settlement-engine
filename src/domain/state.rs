//! Enums from the state diagrams in `docs/ARCHITECTURE.md`. Variant sets are exact.

use serde::{Deserialize, Serialize};

use super::ids::LegId;

/// Which side of the binary contract the *requester* wants on a leg.
///
/// Buying No at `1 - p` is selling Yes at `p`. Prices, collateral, and escrow are always
/// expressed in Yes terms, so the four sides collapse onto [`LegSide::requester_buys_yes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegSide {
    BuyYes,
    SellYes,
    BuyNo,
    SellNo,
}

impl LegSide {
    /// `BuyYes` and `SellNo` make the requester the Yes-buyer; `SellYes` and `BuyNo` make the
    /// maker the Yes-buyer.
    pub const fn requester_buys_yes(self) -> bool {
        matches!(self, LegSide::BuyYes | LegSide::SellNo)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteState {
    Live,
    Selected,
    Locked,
    Released,
}

/// How long after the response deadline the request's contracts resolve. A preset, not a
/// free timestamp: every leg of a request resolves at the same instant, which is what makes
/// one outcome per request coherent, and a preset cannot be malformed or centuries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tenor {
    FiveMinutes,
    TenMinutes,
    OneHour,
    OneDay,
}

impl Tenor {
    pub fn duration(self) -> chrono::Duration {
        match self {
            Tenor::FiveMinutes => chrono::Duration::minutes(5),
            Tenor::TenMinutes => chrono::Duration::minutes(10),
            Tenor::OneHour => chrono::Duration::hours(1),
            Tenor::OneDay => chrono::Duration::days(1),
        }
    }
}

/// What the oracle reports. "Delayed" is the absence of a report, not a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    Rejected,
    AcceptWindowExpired,
    /// `lock_batch` refused because the requester's free balance could not cover their side.
    InsufficientRequesterFunds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leg_side_role() {
        assert!(LegSide::BuyYes.requester_buys_yes());
        assert!(LegSide::SellNo.requester_buys_yes());
        assert!(!LegSide::SellYes.requester_buys_yes());
        assert!(!LegSide::BuyNo.requester_buys_yes());
    }

    #[test]
    fn leg_side_round_trips_through_snake_case_wire_names() {
        for (wire, side) in [
            ("buy_yes", LegSide::BuyYes),
            ("sell_yes", LegSide::SellYes),
            ("buy_no", LegSide::BuyNo),
            ("sell_no", LegSide::SellNo),
        ] {
            let parsed: LegSide = serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(parsed, side);
            assert_eq!(serde_json::to_value(side).unwrap(), wire);
        }
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
