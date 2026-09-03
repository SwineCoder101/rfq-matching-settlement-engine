//! Settlement matrix: every leg side, single- and multi-leg, resolved Yes and No.
//!
//! One winning MM quotes each leg at the case's Yes price, a losing MM quotes 500 bps worse.
//! Each case states only what a reader needs to check by hand: the legs, the outcome, and
//! the requester's net P&L. Everything else (escrow roles and amounts, who got released,
//! conservation) is derived from the arithmetic in the architecture doc.

mod common;

use axum::http::StatusCode;
use common::{TestVenue, assert_quote_states, bal, id_of, leg_ids, ts};
use rstest::rstest;
use serde_json::{Value, json};
use uuid::Uuid;

/// `(side, notional, winning Yes price in bps)`.
type LegSpec = (&'static str, u64, u32);

const START: u64 = 100_000;

/// Sides on which the requester ends up long Yes (and therefore the Yes-buyer in escrow).
fn long_yes(side: &str) -> bool {
    matches!(side, "buy_yes" | "sell_no")
}

#[rstest]
// ---- single leg, notional 1_000 -----------------------------------------------------------
#[case::buy_yes_on_yes(&[("buy_yes", 1_000, 3_500)], "yes", 650)]
#[case::buy_yes_on_no(&[("buy_yes", 1_000, 3_500)], "no", -350)]
#[case::sell_yes_on_yes(&[("sell_yes", 1_000, 3_500)], "yes", -650)]
#[case::sell_yes_on_no(&[("sell_yes", 1_000, 3_500)], "no", 350)]
#[case::buy_no_on_yes(&[("buy_no", 1_000, 4_000)], "yes", -600)]
#[case::buy_no_on_no(&[("buy_no", 1_000, 4_000)], "no", 400)]
#[case::sell_no_on_yes(&[("sell_no", 1_000, 4_000)], "yes", 600)]
#[case::sell_no_on_no(&[("sell_no", 1_000, 4_000)], "no", -400)]
// ---- multi-leg: legs settle independently, P&L is the sum ----------------------------------
#[case::two_legs_on_yes(&[("buy_yes", 1_000, 3_500), ("sell_yes", 2_000, 6_500)], "yes", 650 - 700)]
#[case::two_legs_on_no(&[("buy_yes", 1_000, 3_500), ("sell_yes", 2_000, 6_500)], "no", -350 + 1_300)]
#[case::four_sides_on_yes(
    &[("buy_yes", 1_000, 3_500), ("sell_yes", 2_000, 6_500), ("buy_no", 1_000, 4_000), ("sell_no", 1_000, 2_500)],
    "yes",
    650 - 700 - 600 + 750
)]
#[case::four_sides_on_no(
    &[("buy_yes", 1_000, 3_500), ("sell_yes", 2_000, 6_500), ("buy_no", 1_000, 4_000), ("sell_no", 1_000, 2_500)],
    "no",
    -350 + 1_300 + 400 - 250
)]
// ---- rounding: odd notional, remainder lands on the Yes-seller -----------------------------
#[case::odd_notional_on_yes(&[("buy_yes", 7, 3_333)], "yes", 7 - 2)]
#[case::odd_notional_on_no(&[("buy_yes", 7, 3_333)], "no", -2)]
#[tokio::test]
async fn settles(#[case] legs: &[LegSpec], #[case] outcome: &str, #[case] requester_pnl: i64) {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    let winner = Uuid::new_v4();
    let loser = Uuid::new_v4();
    for p in [requester, winner, loser] {
        v.fund(p, START).await;
    }

    // ---- open ------------------------------------------------------------------------------
    let leg_bodies: Vec<Value> = legs
        .iter()
        .enumerate()
        .map(|(i, (side, notional, _))| {
            json!({ "contract": format!("C{i}"), "description": format!("Settles Yes if index C{i} closes above the strike 100.00 per the venue's published source at resolution; otherwise No."), "side": side, "notional": notional })
        })
        .collect();
    let created = v
        .create_request_body(
            requester,
            json!({ "legs": leg_bodies, "tenor": "five_minutes", "response_deadline": ts(v.at(30)) }),
        )
        .await;
    let request_id = id_of(&created);
    let leg_ids = leg_ids(&created);

    // ---- quote: winner at the case price, loser 500 bps worse ------------------------------
    let mut winning_quotes = Vec::new();
    let mut losing_quotes = Vec::new();
    let mut expected_escrows = Vec::new();
    let (mut requester_lock, mut winner_lock) = (0, 0);
    for ((side, notional, price), leg_id) in legs.iter().zip(&leg_ids) {
        let worse = if long_yes(side) {
            price + 500
        } else {
            price - 500
        };
        losing_quotes.push(
            v.quote_ok(loser, &request_id, leg_id, worse, *notional, v.at(600))
                .await,
        );
        winning_quotes.push(
            v.quote_ok(winner, &request_id, leg_id, *price, *notional, v.at(600))
                .await,
        );

        // Yes-buyer locks p * n (truncated); Yes-seller locks the remainder.
        let yes_buyer_amount = notional * u64::from(*price) / 10_000;
        let yes_seller_amount = notional - yes_buyer_amount;
        let (yes_buyer, yes_seller, requester_amount, winner_amount) = if long_yes(side) {
            (requester, winner, yes_buyer_amount, yes_seller_amount)
        } else {
            (winner, requester, yes_seller_amount, yes_buyer_amount)
        };
        requester_lock += requester_amount;
        winner_lock += winner_amount;
        expected_escrows.push(json!({
            "leg_id": leg_id, "yes_buyer": yes_buyer, "yes_seller": yes_seller,
            "yes_buyer_amount": yes_buyer_amount, "yes_seller_amount": yes_seller_amount, "notional": notional,
        }));
    }
    let loser_lock: u64 = START - v.balances(loser).await.free;
    assert!(loser_lock > 0, "the losing MM posted collateral too");

    // ---- present: the winner is selected on every leg ---------------------------------------
    v.advance_to(30).await;
    let presented = v.snapshot(&request_id).await;
    assert_eq!(presented["state"], "presented", "{presented}");
    let selected: Vec<String> = presented["package"]["selections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["quote_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(selected, winning_quotes, "best quote per leg, in leg order");

    // ---- accept: escrow per leg, loser released ---------------------------------------------
    let (status, locked) = v.accept(requester, &request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["escrows"], json!(expected_escrows));
    for q in &winning_quotes {
        assert_quote_states(&locked, &[(q, "locked")]);
    }
    for q in &losing_quotes {
        assert_quote_states(&locked, &[(q, "released")]);
    }
    v.assert_balances(&[
        (
            "requester",
            requester,
            bal(START - requester_lock, 0, requester_lock),
        ),
        ("winner", winner, bal(START - winner_lock, 0, winner_lock)),
        ("loser", loser, bal(START, 0, 0)),
    ])
    .await;

    // ---- resolve: zero-sum between requester and winner; loser untouched --------------------
    let (status, settled) = v.resolve(&request_id, outcome).await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    let after = |pnl: i64| u64::try_from(i64::try_from(START).unwrap() + pnl).unwrap();
    v.assert_balances(&[
        ("requester", requester, bal(after(requester_pnl), 0, 0)),
        ("winner", winner, bal(after(-requester_pnl), 0, 0)),
        ("loser", loser, bal(START, 0, 0)),
    ])
    .await;
    v.assert_conserved().await;
}
