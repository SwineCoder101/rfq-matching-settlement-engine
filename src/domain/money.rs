//! Money and price primitives. Integer math only; no `f64` anywhere near funds.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use serde::Serialize;

/// Minor units (e.g. cents). Never negative.
///
/// `Add`/`Sub` panic on overflow/underflow in every build profile: silently wrapping money is
/// worse than crashing. Use the `checked_*` forms where overflow is a recoverable condition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Amount(u64);

impl Amount {
    pub const ZERO: Amount = Amount(0);

    pub const fn new(minor_units: u64) -> Self {
        Self(minor_units)
    }

    pub const fn minor_units(self) -> u64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn checked_add(self, rhs: Amount) -> Option<Amount> {
        self.0.checked_add(rhs.0).map(Amount)
    }

    pub fn checked_sub(self, rhs: Amount) -> Option<Amount> {
        self.0.checked_sub(rhs.0).map(Amount)
    }
}

impl Add for Amount {
    type Output = Amount;

    fn add(self, rhs: Amount) -> Amount {
        self.checked_add(rhs).expect("Amount overflow")
    }
}

impl Sub for Amount {
    type Output = Amount;

    fn sub(self, rhs: Amount) -> Amount {
        self.checked_sub(rhs).expect("Amount underflow")
    }
}

impl AddAssign for Amount {
    fn add_assign(&mut self, rhs: Amount) {
        *self = *self + rhs;
    }
}

impl SubAssign for Amount {
    fn sub_assign(&mut self, rhs: Amount) {
        *self = *self - rhs;
    }
}

impl Sum for Amount {
    fn sum<I: Iterator<Item = Amount>>(iter: I) -> Amount {
        iter.fold(Amount::ZERO, Add::add)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Probability-style Yes price of a binary contract in basis points, `1..=9_999`.
///
/// `0` and `10_000` are rejected: a binary contract at 0% or 100% is not a trade. Ordering is
/// numeric, so `min`/`max` give the cheapest/dearest Yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Price(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("price {bps} bps is outside 1..=9999")]
pub struct InvalidPrice {
    pub bps: u32,
}

impl Price {
    const SCALE: u32 = 10_000;

    pub const fn new(bps: u32) -> Result<Self, InvalidPrice> {
        if bps < 1 || bps >= Self::SCALE {
            return Err(InvalidPrice { bps });
        }
        Ok(Self(bps))
    }

    pub const fn bps(self) -> u32 {
        self.0
    }

    /// What the Yes-buyer locks: `notional * p / 10_000`, truncated toward zero. Computed in
    /// `u128` so it cannot overflow; the quotient is strictly below `notional` because
    /// `p < 10_000`.
    pub fn yes_buyer_lock(self, notional: Amount) -> Amount {
        let scaled =
            u128::from(notional.minor_units()) * u128::from(self.0) / u128::from(Self::SCALE);
        Amount::new(u64::try_from(scaled).expect("p/10_000 < 1 so buyer lock fits in u64"))
    }

    /// What the Yes-seller locks. Always derived as `notional - yes_buyer_lock` so the two
    /// sides sum to `notional` exactly; any rounding remainder lands on the seller.
    pub fn yes_seller_lock(self, notional: Amount) -> Amount {
        notional - self.yes_buyer_lock(notional)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_rejects_zero_and_full() {
        assert_eq!(Price::new(0), Err(InvalidPrice { bps: 0 }));
        assert_eq!(Price::new(10_000), Err(InvalidPrice { bps: 10_000 }));
        assert_eq!(Price::new(10_001), Err(InvalidPrice { bps: 10_001 }));
        assert_eq!(Price::new(1).unwrap().bps(), 1);
        assert_eq!(Price::new(9_999).unwrap().bps(), 9_999);
    }

    #[test]
    fn buyer_lock_is_truncated_share_of_notional() {
        let p = Price::new(2_500).unwrap();
        assert_eq!(p.yes_buyer_lock(Amount::new(10_000)), Amount::new(2_500));
        assert_eq!(p.yes_seller_lock(Amount::new(10_000)), Amount::new(7_500));
        // 7 * 2500 / 10000 = 1.75 -> 1; seller absorbs the remainder.
        assert_eq!(p.yes_buyer_lock(Amount::new(7)), Amount::new(1));
        assert_eq!(p.yes_seller_lock(Amount::new(7)), Amount::new(6));
    }

    #[test]
    fn buyer_plus_seller_equals_notional_for_sweep() {
        let prices = [
            1, 2, 3, 7, 9, 99, 100, 1_234, 5_000, 6_667, 9_990, 9_998, 9_999,
        ];
        let notionals = [
            1,
            2,
            3,
            7,
            9_999,
            10_000,
            10_001,
            123_456_789,
            u64::MAX / 2,
            u64::MAX,
        ];
        for &bps in &prices {
            let p = Price::new(bps).unwrap();
            for &n in &notionals {
                let notional = Amount::new(n);
                let buyer = p.yes_buyer_lock(notional);
                let seller = p.yes_seller_lock(notional);
                assert_eq!(
                    buyer + seller,
                    notional,
                    "p={bps} n={n}: {buyer} + {seller} != {notional}"
                );
                assert!(
                    buyer < notional,
                    "buyer never locks the full notional (p < 100%)"
                );
            }
        }
    }

    #[test]
    fn amount_arithmetic() {
        assert_eq!(Amount::new(5) + Amount::new(7), Amount::new(12));
        assert_eq!(Amount::new(7) - Amount::new(5), Amount::new(2));
        assert_eq!(Amount::new(u64::MAX).checked_add(Amount::new(1)), None);
        assert_eq!(Amount::new(1).checked_sub(Amount::new(2)), None);
        assert_eq!(
            [Amount::new(1), Amount::new(2)].into_iter().sum::<Amount>(),
            Amount::new(3)
        );
    }

    #[test]
    #[should_panic(expected = "Amount underflow")]
    fn amount_sub_underflow_panics() {
        let _ = Amount::new(1) - Amount::new(2);
    }
}
