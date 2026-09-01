//! Best-quote selection. Pure: no I/O, no clock, no ledger.

use std::cmp::Reverse;

use chrono::{DateTime, Utc};

use super::ids::QuoteId;
use super::request::{Leg, Quote};
use super::state::QuoteState;

/// Pick the best eligible quote for `leg`, if any.
///
/// Eligible: `state == Live`, `leg_id == leg.id`, `size >= leg.notional`, `expires_at > now`,
/// and `expires_at >= accept_deadline` (the quote must survive the whole accept window).
///
/// Prices are Yes prices. A requester who ends up long Yes (`BuyYes`, `SellNo`) wants the
/// lowest; one who ends up short Yes (`SellYes`, `BuyNo`) wants the highest. Ties break on
/// lowest `seq` — the engine's monotonic submit order — not on `submitted_at`.
pub fn select_best(
    leg: &Leg,
    quotes: &[Quote],
    now: DateTime<Utc>,
    accept_deadline: DateTime<Utc>,
) -> Option<QuoteId> {
    let eligible = quotes.iter().filter(|q| {
        q.state == QuoteState::Live
            && q.leg_id == leg.id
            && q.size >= leg.notional
            && q.expires_at > now
            && q.expires_at >= accept_deadline
    });

    let best = if leg.side.requester_buys_yes() {
        eligible.min_by_key(|q| (q.price, q.seq))
    } else {
        eligible.min_by_key(|q| (Reverse(q.price), q.seq))
    };
    best.map(|q| q.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Amount, ContractDescription, ContractId, LegId, LegSide, PartyId, Price, Seq};

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    const NOW: i64 = 100;
    const ACCEPT_DEADLINE: i64 = 200;

    fn leg(side: LegSide) -> Leg {
        Leg::new(
            ContractId::new("C").unwrap(),
            ContractDescription::new("C").unwrap(),
            side,
            Amount::new(1_000),
        )
        .unwrap()
    }

    struct Q {
        leg_id: LegId,
        bps: u32,
        seq: u64,
        size: u64,
        expires_at: i64,
        submitted_at: i64,
        state: QuoteState,
    }

    fn q(leg: &Leg, bps: u32, seq: u64) -> Q {
        Q {
            leg_id: leg.id,
            bps,
            seq,
            size: 1_000,
            expires_at: 300,
            submitted_at: seq as i64,
            state: QuoteState::Live,
        }
    }

    fn build(q: Q) -> Quote {
        Quote {
            id: QuoteId::new(),
            leg_id: q.leg_id,
            maker: PartyId::new(),
            price: Price::new(q.bps).unwrap(),
            size: Amount::new(q.size),
            expires_at: t(q.expires_at),
            submitted_at: t(q.submitted_at),
            seq: Seq::new(q.seq),
            state: q.state,
        }
    }

    fn run(leg: &Leg, qs: Vec<Q>) -> (Vec<Quote>, Option<QuoteId>) {
        let quotes: Vec<Quote> = qs.into_iter().map(build).collect();
        let best = select_best(leg, &quotes, t(NOW), t(ACCEPT_DEADLINE));
        (quotes, best)
    }

    #[test]
    fn empty_has_no_best() {
        let leg = leg(LegSide::BuyYes);
        assert_eq!(select_best(&leg, &[], t(NOW), t(ACCEPT_DEADLINE)), None);
    }

    #[test]
    fn all_expired_has_no_best() {
        let leg = leg(LegSide::BuyYes);
        let (_, best) = run(
            &leg,
            vec![
                Q { expires_at: 50, ..q(&leg, 4_000, 1) },
                Q { expires_at: NOW, ..q(&leg, 3_000, 2) }, // expires_at == now is expired
            ],
        );
        assert_eq!(best, None);
    }

    #[test]
    fn expiring_inside_accept_window_is_ineligible() {
        let leg = leg(LegSide::BuyYes);
        let (quotes, best) = run(
            &leg,
            vec![
                Q { expires_at: ACCEPT_DEADLINE - 1, ..q(&leg, 1_000, 1) }, // cheapest but too short
                Q { expires_at: ACCEPT_DEADLINE, ..q(&leg, 2_000, 2) },     // exactly at deadline: ok
            ],
        );
        assert_eq!(best, Some(quotes[1].id));
    }

    #[test]
    fn size_too_small_is_ineligible() {
        let leg = leg(LegSide::BuyYes);
        let (quotes, best) = run(
            &leg,
            vec![
                Q { size: 999, ..q(&leg, 1_000, 1) }, // cheapest but undersized
                Q { size: 1_000, ..q(&leg, 5_000, 2) },
            ],
        );
        assert_eq!(best, Some(quotes[1].id));

        let (_, best) = run(&leg, vec![Q { size: 1, ..q(&leg, 1_000, 1) }]);
        assert_eq!(best, None);
    }

    #[test]
    fn long_yes_sides_take_lowest_yes_price() {
        for side in [LegSide::BuyYes, LegSide::SellNo] {
            let leg = leg(side);
            let (quotes, best) = run(&leg, vec![q(&leg, 4_000, 1), q(&leg, 2_500, 2), q(&leg, 3_000, 3)]);
            assert_eq!(best, Some(quotes[1].id), "side {side:?}");
        }
    }

    #[test]
    fn short_yes_sides_take_highest_yes_price() {
        for side in [LegSide::SellYes, LegSide::BuyNo] {
            let leg = leg(side);
            let (quotes, best) = run(&leg, vec![q(&leg, 4_000, 1), q(&leg, 2_500, 2), q(&leg, 3_000, 3)]);
            assert_eq!(best, Some(quotes[0].id), "side {side:?}");
        }
    }

    #[test]
    fn price_tie_breaks_on_lowest_seq_not_submitted_at() {
        for side in LegSide::ALL {
            let leg = leg(side);
            // seq 2 has the *earlier* submitted_at timestamp; seq 1 must still win.
            let (quotes, best) = run(
                &leg,
                vec![
                    Q { submitted_at: 90, ..q(&leg, 3_000, 2) },
                    Q { submitted_at: 95, ..q(&leg, 3_000, 1) },
                ],
            );
            assert_eq!(best, Some(quotes[1].id), "side {side:?}");
        }
    }

    #[test]
    fn non_live_quotes_are_ineligible() {
        let leg = leg(LegSide::BuyYes);
        let (quotes, best) = run(
            &leg,
            vec![
                Q { state: QuoteState::Released, ..q(&leg, 1_000, 1) },
                Q { state: QuoteState::Selected, ..q(&leg, 1_500, 2) },
                Q { state: QuoteState::Locked, ..q(&leg, 2_000, 3) },
                q(&leg, 4_000, 4),
            ],
        );
        assert_eq!(best, Some(quotes[3].id));
    }

    #[test]
    fn quotes_on_other_legs_are_ignored() {
        let leg = leg(LegSide::BuyYes);
        let other = self::leg(LegSide::BuyYes);
        let (_, best) = run(&leg, vec![q(&other, 1_000, 1)]);
        assert_eq!(best, None);
    }
}
