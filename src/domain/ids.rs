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
            /// Mint a fresh random id.
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
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
    /// A participant: requester, market maker, or oracle operator. Claimed via `x-party-id`.
    PartyId
);
uuid_id!(
    /// An `RfqRequest` aggregate.
    RequestId
);
uuid_id!(
    /// One leg of a request.
    LegId
);
uuid_id!(
    /// One market-maker quote on a leg.
    QuoteId
);

/// Opaque identifier of a binary contract. The engine never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ContractId(String);

/// A contract id must be non-empty and not just whitespace.
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

impl TryFrom<String> for ContractId {
    type Error = InvalidContractId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ContractId {
    type Error = InvalidContractId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for ContractId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Engine-assigned monotonic sequence number.
///
/// Stamped on every quote by the engine actor at submit time. Because the actor serializes all
/// mutations, `Seq` is a total order over quotes that does not depend on clock resolution or
/// skew — which is why matching breaks price ties on `Seq`, not `submitted_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    /// The next sequence number. Panics on `u64` overflow, which cannot happen in practice.
    pub fn next(self) -> Self {
        Self(self.0.checked_add(1).expect("Seq overflow"))
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_id_rejects_empty_and_whitespace() {
        assert_eq!(ContractId::new(""), Err(InvalidContractId));
        assert_eq!(ContractId::new("   "), Err(InvalidContractId));
        assert_eq!(ContractId::new("BTC-100K-2026").unwrap().as_str(), "BTC-100K-2026");
    }

    #[test]
    fn seq_is_monotonic() {
        let a = Seq::ZERO;
        let b = a.next();
        assert!(b > a);
        assert_eq!(b.value(), 1);
    }

    #[test]
    fn distinct_id_types_do_not_compare_equal_by_accident() {
        // Compile-time property: PartyId and LegId are distinct types. This just checks
        // the round trip through Uuid works.
        let u = Uuid::new_v4();
        assert_eq!(PartyId::from(u).as_uuid(), u);
        assert_eq!(LegId::from_uuid(u).as_uuid(), u);
    }
}
