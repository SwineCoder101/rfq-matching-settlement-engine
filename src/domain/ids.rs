//! Identity newtypes. No `String` ids anywhere in the domain.

use std::fmt;

use serde::Serialize;
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id!(
    /// A participant: requester or market maker. Claimed via `x-party-id`.
    PartyId
);
uuid_id!(RequestId);
uuid_id!(LegId);
uuid_id!(QuoteId);

/// Opaque identifier of a binary contract. The engine never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContractId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("contract id must be non-empty")]
pub struct InvalidContractId;

impl ContractId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidContractId> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(InvalidContractId);
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Human-readable statement of what the contract resolves on. Carried for participants; the
/// engine never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContractDescription(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidContractDescription {
    #[error("contract description must be non-empty")]
    Empty,
    #[error(
        "contract description exceeds {} characters",
        ContractDescription::MAX_CHARS
    )]
    TooLong,
}

impl ContractDescription {
    pub const MAX_CHARS: usize = 1_000;

    pub fn new(text: impl Into<String>) -> Result<Self, InvalidContractDescription> {
        let text = text.into();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(InvalidContractDescription::Empty);
        }
        if trimmed.chars().count() > Self::MAX_CHARS {
            return Err(InvalidContractDescription::TooLong);
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Engine-assigned submit order. The actor serializes all mutations, so `Seq` is a total
/// order over quotes independent of clock resolution or skew; matching breaks price ties on
/// it, not on `submitted_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn next(self) -> Self {
        Self(self.0.checked_add(1).expect("Seq overflow"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_id_rejects_empty_and_whitespace() {
        assert_eq!(ContractId::new(""), Err(InvalidContractId));
        assert_eq!(ContractId::new("   "), Err(InvalidContractId));
        assert_eq!(
            ContractId::new("BTC-100K-2026").unwrap().as_str(),
            "BTC-100K-2026"
        );
    }

    #[test]
    fn contract_description_is_trimmed_and_bounded() {
        assert_eq!(
            ContractDescription::new("  "),
            Err(InvalidContractDescription::Empty)
        );
        assert_eq!(
            ContractDescription::new("x".repeat(ContractDescription::MAX_CHARS + 1)),
            Err(InvalidContractDescription::TooLong)
        );
        assert_eq!(
            ContractDescription::new("  BTC > 100k  ").unwrap().as_str(),
            "BTC > 100k"
        );
        assert!(ContractDescription::new("x".repeat(ContractDescription::MAX_CHARS)).is_ok());
    }

    #[test]
    fn seq_is_monotonic() {
        let a = Seq::ZERO;
        let b = a.next();
        assert!(b > a);
        assert_eq!(b, Seq::new(1));
    }
}
