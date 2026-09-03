//! Parlay settlement — RED. These tests define the payout model before the engine implements it.
//!
//! # Model: sequential accumulator over the request's leg order
//!
//! * The request carries one `stake` `S`; legs carry no notional.
//! * A maker quotes a Yes price `p` (bps) and a `size` = the most collateral it will post.
//!   `size` is reserved at submit.
//! * `q_k` = probability the requester's side of leg `k` hits, as priced:
//!   long-Yes legs (`buy_yes`, `sell_no`) `q = p`; short-Yes legs (`sell_yes`, `buy_no`) `q = 10_000 − p`.
//!   Best quote per leg = lowest `q` (largest payout), ties on `seq`. Legs are matched in request order.
//! * Pot chain, integer floor: `pot_0 = S`, `pot_k = pot_{k−1} · 10_000 / q_k`.
//!   Maker `k` collateral `c_k = pot_k − pot_{k−1}`. Payout `N = pot_n`. Escrow pool = `S + Σ c_k = N`.
//! * A quote is eligible only if `size ≥ c_k` given the pots chosen for earlier legs.
//! * Accept: one `lock_batch` — `S` from the requester's free balance, `c_k` from each selected
//!   maker's reservation; the rest of each reservation (`size − c_k`) returns to free.
//! * Resolution is per leg: `POST /v1/oracle/resolve { request_id, leg_id, outcome }`.
//!   The request settles the moment the result is determined, scanning legs in order:
//!   first resolved leg that is *unfavourable*, with every earlier leg favourable → that maker
//!   receives `pot_k` (its collateral plus everything rolled in) and later makers get `c_j` back;
//!   every leg favourable → the requester receives `N`;
//!   an unresolved leg before any unfavourable one → stay `Locked`.
//!   `Invalid` on any leg → `Unwound`, everyone refunded. `Disputed` → `Disputed`.
//!
//! Worked example used below: `S = 1_000`, three `buy_yes` legs at 50 %:
//! `pot = 1_000 → 2_000 → 4_000 → 8_000`, `c = 1_000, 2_000, 4_000`, `N = 8_000` (7-to-1).

mod common;

use axum::http::StatusCode;
use common::{QUOTE_EXPIRY_SECS, RESPONSE_DEADLINE_SECS, TestVenue, bal, id_of, leg_ids, pleg};
use rstest::rstest;
use serde_json::{Value, json};
use uuid::Uuid;

const STAKE: u64 = 1_000;

/// `(side, yes price bps, maker size)` — the maker is funded exactly `size`.
type LegSpec = (&'static str, u32, u64);

struct Parlay {
    requester: Uuid,
    makers: Vec<Uuid>,
    request_id: String,
    leg_ids: Vec<String>,
    quotes: Vec<String>,
}

/// Fund R with `requester_funds`, open a parlay of `legs`, have one maker per leg quote it, and
/// tick to the response deadline. Returns everything the assertions need.
async fn presented(v: &TestVenue, requester_funds: u64, legs: &[LegSpec]) -> Parlay {
    let requester = Uuid::new_v4();
    v.fund(requester, requester_funds).await;
    let leg_bodies: Vec<Value> = legs.iter().enumerate().map(|(i, (side, _, _))| pleg(side, &format!("C{i}"))).collect();
    let (status, created) = v.open_parlay(requester, STAKE, json!(leg_bodies), v.at(RESPONSE_DEADLINE_SECS)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["stake"], STAKE);
    assert!(created["payout"].is_null(), "no payout until a package is presented");
    let request_id = id_of(&created);
    let leg_ids = leg_ids(&created);

    let mut makers = Vec::new();
    let mut quotes = Vec::new();
    for ((_, price, size), leg_id) in legs.iter().zip(&leg_ids) {
        let maker = Uuid::new_v4();
        v.fund(maker, *size).await;
        quotes.push(v.quote_ok(maker, &request_id, leg_id, *price, *size, v.at(QUOTE_EXPIRY_SECS)).await);
        assert_eq!(v.balances(maker).await, bal(0, *size, 0), "size is reserved at submit");
        makers.push(maker);
    }
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    Parlay { requester, makers, request_id, leg_ids, quotes }
}

fn selections(r: &Value) -> Vec<(String, u64)> {
    r["package"]["selections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["quote_id"].as_str().unwrap().to_owned(), s["collateral"].as_u64().unwrap()))
        .collect()
}

async fn accept_ok(v: &TestVenue, p: &Parlay) -> Value {
    let (status, locked) = v.accept(p.requester, &p.request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["state"], "locked");
    locked
}

async fn resolve_ok(v: &TestVenue, p: &Parlay, leg: usize, outcome: &str) -> Value {
    let (status, r) = v.resolve_leg(v.oracle_party(), &p.request_id, &p.leg_ids[leg], outcome).await;
    assert_eq!(status, StatusCode::OK, "resolve leg {leg} {outcome}: {r}");
    assert_eq!(r["legs"][leg]["outcome"], outcome);
    r
}

/// Net change in free balance once nothing is reserved or escrowed.
async fn net(v: &TestVenue, party: Uuid, funded: u64) -> i64 {
    let b = v.balances(party).await;
    assert_eq!((b.reserved, b.escrowed), (0, 0), "party {party} still has funds held");
    i64::try_from(b.free).unwrap() - i64::try_from(funded).unwrap()
}

// =============================================================================================
// Pricing: payout = stake / Π q, collateral per maker = its increment of the pot
// =============================================================================================

#[tokio::test]
async fn parlay_single_leg_pays_stake_over_probability() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000)]).await;
    let r = v.snapshot(&p.request_id).await;
    assert_eq!(r["state"], "presented");
    assert_eq!(r["payout"], 2_000, "1_000 / 0.5");
    assert_eq!(selections(&r), vec![(p.quotes[0].clone(), 1_000)]);

    accept_ok(&v, &p).await;
    v.assert_balances(&[("requester", p.requester, bal(0, 0, STAKE)), ("maker", p.makers[0], bal(0, 0, 1_000))]).await;
    v.assert_conserved().await;

    let settled = resolve_ok(&v, &p, 0, "yes").await;
    assert_eq!(settled["state"], "settled");
    assert_eq!(settled["winner"], p.requester.to_string());
    assert_eq!(net(&v, p.requester, STAKE).await, 1_000);
    assert_eq!(net(&v, p.makers[0], 1_000).await, -1_000);
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_short_yes_leg_uses_complement_probability() {
    let v = TestVenue::new();
    // buy_no at Yes 40 % → q = 60 % → pot = floor(1_000 · 10_000 / 6_000) = 1_666, c = 666.
    let p = presented(&v, STAKE, &[("buy_no", 4_000, 666)]).await;
    let r = v.snapshot(&p.request_id).await;
    assert_eq!(r["payout"], 1_666);
    assert_eq!(selections(&r), vec![(p.quotes[0].clone(), 666)]);

    accept_ok(&v, &p).await;
    let settled = resolve_ok(&v, &p, 0, "no").await; // No is favourable to a No-buyer
    assert_eq!(settled["state"], "settled");
    assert_eq!(settled["winner"], p.requester.to_string());
    assert_eq!(net(&v, p.requester, STAKE).await, 666);
    assert_eq!(net(&v, p.makers[0], 666).await, -666);
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_three_legs_all_hit_pays_the_product() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000), ("buy_yes", 5_000, 4_000)]).await;
    let r = v.snapshot(&p.request_id).await;
    assert_eq!(r["payout"], 8_000, "1_000 / 0.5³");
    assert_eq!(
        selections(&r),
        vec![(p.quotes[0].clone(), 1_000), (p.quotes[1].clone(), 2_000), (p.quotes[2].clone(), 4_000)],
        "each maker covers its increment of the pot"
    );

    accept_ok(&v, &p).await;
    v.assert_balances(&[
        ("requester", p.requester, bal(0, 0, STAKE)),
        ("m1", p.makers[0], bal(0, 0, 1_000)),
        ("m2", p.makers[1], bal(0, 0, 2_000)),
        ("m3", p.makers[2], bal(0, 0, 4_000)),
    ])
    .await;
    assert_eq!(v.ledger.escrowed_total().minor_units(), 8_000, "pool equals payout");
    v.assert_conserved().await;

    resolve_ok(&v, &p, 0, "yes").await;
    resolve_ok(&v, &p, 1, "yes").await;
    assert_eq!(v.snapshot(&p.request_id).await["state"], "locked", "undetermined until the last leg");
    let settled = resolve_ok(&v, &p, 2, "yes").await;
    assert_eq!(settled["state"], "settled");
    assert_eq!(settled["winner"], p.requester.to_string());
    assert_eq!(net(&v, p.requester, STAKE).await, 7_000, "7-to-1");
    assert_eq!(net(&v, p.makers[0], 1_000).await, -1_000);
    assert_eq!(net(&v, p.makers[1], 2_000).await, -2_000);
    assert_eq!(net(&v, p.makers[2], 4_000).await, -4_000);
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_mixed_sides_chain_with_floor_rounding() {
    let v = TestVenue::new();
    // q = 50 %, 25 %, 60 %: pot 1_000 → 2_000 → 8_000 → 13_333; c = 1_000, 6_000, 5_333.
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("sell_yes", 7_500, 6_000), ("buy_no", 4_000, 5_333)]).await;
    let r = v.snapshot(&p.request_id).await;
    assert_eq!(r["payout"], 13_333);
    assert_eq!(selections(&r).iter().map(|(_, c)| *c).collect::<Vec<_>>(), vec![1_000, 6_000, 5_333]);

    accept_ok(&v, &p).await;
    assert_eq!(v.ledger.escrowed_total().minor_units(), 13_333);
    resolve_ok(&v, &p, 0, "yes").await;
    resolve_ok(&v, &p, 1, "no").await;
    let settled = resolve_ok(&v, &p, 2, "no").await;
    assert_eq!(settled["winner"], p.requester.to_string());
    assert_eq!(net(&v, p.requester, STAKE).await, 12_333);
    v.assert_conserved().await;
}

// =============================================================================================
// Settlement: the first unfavourable leg in request order takes the pot
// =============================================================================================

#[rstest]
#[case::miss_on_leg_1(0, [-1_000, 1_000, 0, 0])]
#[case::miss_on_leg_2(1, [-1_000, -1_000, 2_000, 0])]
#[case::miss_on_leg_3(2, [-1_000, -1_000, -2_000, 4_000])]
#[tokio::test]
async fn parlay_first_miss_in_leg_order_takes_the_pot(#[case] miss: usize, #[case] expected_net: [i64; 4]) {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000), ("buy_yes", 5_000, 4_000)]).await;
    accept_ok(&v, &p).await;

    // Resolve in reverse order so settlement cannot depend on arrival order.
    let mut last = Value::Null;
    for leg in (0..3).rev() {
        last = resolve_ok(&v, &p, leg, if leg == miss { "no" } else { "yes" }).await;
    }
    assert_eq!(last["state"], "settled");
    assert_eq!(last["winner"], p.makers[miss].to_string());

    let funded = [STAKE, 1_000, 2_000, 4_000];
    let parties = [p.requester, p.makers[0], p.makers[1], p.makers[2]];
    for i in 0..4 {
        assert_eq!(net(&v, parties[i], funded[i]).await, expected_net[i], "party {i}");
    }
    assert_eq!(expected_net.iter().sum::<i64>(), 0, "zero-sum");
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_settles_as_soon_as_the_result_is_determined() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000), ("buy_yes", 5_000, 4_000)]).await;
    accept_ok(&v, &p).await;

    // Leg 2 misses, but leg 1 is still open: if leg 1 also misses, maker 1 wins instead. Wait.
    let r = resolve_ok(&v, &p, 1, "no").await;
    assert_eq!(r["state"], "locked");
    assert_eq!(v.balances(p.makers[1]).await, bal(0, 0, 2_000), "no payout while undetermined");

    // Leg 1 hits → maker 2 is now certainly the winner; leg 3 never needs to resolve.
    let settled = resolve_ok(&v, &p, 0, "yes").await;
    assert_eq!(settled["state"], "settled");
    assert_eq!(settled["winner"], p.makers[1].to_string());
    assert!(settled["legs"][2]["outcome"].is_null());
    assert_eq!(net(&v, p.makers[1], 2_000).await, 2_000, "pot_2 = own 2_000 + rolled 2_000");
    assert_eq!(net(&v, p.makers[2], 4_000).await, 0, "unplayed leg refunded");
    assert_eq!(v.resolve_leg(v.oracle_party(), &p.request_id, &p.leg_ids[2], "yes").await.0, StatusCode::CONFLICT, "terminal");
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_first_leg_miss_settles_immediately() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000), ("buy_yes", 5_000, 4_000)]).await;
    accept_ok(&v, &p).await;

    let settled = resolve_ok(&v, &p, 0, "no").await;
    assert_eq!(settled["state"], "settled");
    assert_eq!(settled["winner"], p.makers[0].to_string());
    assert_eq!(net(&v, p.requester, STAKE).await, -1_000);
    assert_eq!(net(&v, p.makers[0], 1_000).await, 1_000);
    assert_eq!(net(&v, p.makers[1], 2_000).await, 0);
    assert_eq!(net(&v, p.makers[2], 4_000).await, 0);
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_invalid_leg_unwinds_everyone() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000), ("buy_yes", 5_000, 4_000)]).await;
    accept_ok(&v, &p).await;
    resolve_ok(&v, &p, 0, "yes").await;

    let unwound = resolve_ok(&v, &p, 1, "invalid").await;
    assert_eq!(unwound["state"], "unwound");
    assert!(unwound["winner"].is_null());
    assert_eq!(net(&v, p.requester, STAKE).await, 0);
    for (m, funded) in p.makers.iter().zip([1_000, 2_000, 4_000]) {
        assert_eq!(net(&v, *m, funded).await, 0, "everyone gets exactly their own money back");
    }
    v.assert_conserved().await;
}

// =============================================================================================
// Collateral: size caps eligibility; excess reservation is released at accept
// =============================================================================================

#[tokio::test]
async fn parlay_quote_with_size_below_required_collateral_is_ineligible() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    v.fund(requester, STAKE).await;
    let (status, created) = v.open_parlay(requester, STAKE, json!([pleg("buy_yes", "C0")]), v.at(RESPONSE_DEADLINE_SECS)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let request_id = id_of(&created);
    let leg_id = leg_ids(&created).remove(0);

    // Better price but can only post 500 of the 1_000 that 50 % requires.
    let small = Uuid::new_v4();
    v.fund(small, 500).await;
    let q_small = v.quote_ok(small, &request_id, &leg_id, 5_000, 500, v.at(QUOTE_EXPIRY_SECS)).await;
    // Worse price (60 % → c = 666) with enough size.
    let big = Uuid::new_v4();
    v.fund(big, 1_000).await;
    let q_big = v.quote_ok(big, &request_id, &leg_id, 6_000, 1_000, v.at(QUOTE_EXPIRY_SECS)).await;

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let r = v.snapshot(&request_id).await;
    assert_eq!(r["state"], "presented", "{r}");
    assert_eq!(r["payout"], 1_666);
    assert_eq!(selections(&r), vec![(q_big.clone(), 666)]);
    assert_eq!(common::quote_state(&r, &q_small), "live");
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_only_undersized_quotes_leaves_leg_unmatched() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    v.fund(requester, STAKE).await;
    let (_, created) = v.open_parlay(requester, STAKE, json!([pleg("buy_yes", "C0")]), v.at(RESPONSE_DEADLINE_SECS)).await;
    let request_id = id_of(&created);
    let leg_id = leg_ids(&created).remove(0);
    let small = Uuid::new_v4();
    v.fund(small, 500).await;
    v.quote_ok(small, &request_id, &leg_id, 5_000, 500, v.at(QUOTE_EXPIRY_SECS)).await;

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let r = v.snapshot(&request_id).await;
    assert_eq!(r["state"], "failed");
    assert_eq!(r["fail_reason"], json!({ "reason": "leg_unmatched", "leg_id": leg_id }));
    assert_eq!(v.balances(small).await, bal(500, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_excess_reservation_is_released_at_accept() {
    let v = TestVenue::new();
    // Maker posts size 3_000 on a 50 % leg that only needs 1_000.
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 3_000)]).await;
    assert_eq!(v.balances(p.makers[0]).await, bal(0, 3_000, 0), "full size stays reserved while Presented");

    accept_ok(&v, &p).await;
    v.assert_balances(&[("requester", p.requester, bal(0, 0, STAKE)), ("maker", p.makers[0], bal(2_000, 0, 1_000))]).await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn parlay_requester_locks_exactly_the_stake() {
    let v = TestVenue::new();
    // Funded one short of the stake → accept is refused and the maker is released.
    let short = presented(&v, STAKE - 1, &[("buy_yes", 5_000, 1_000)]).await;
    let (status, body) = v.accept(short.requester, &short.request_id).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    let r = v.snapshot(&short.request_id).await;
    assert_eq!(r["state"], "failed");
    assert_eq!(r["fail_reason"], json!({ "reason": "insufficient_requester_funds" }));
    v.assert_balances(&[("requester", short.requester, bal(STAKE - 1, 0, 0)), ("maker", short.makers[0], bal(1_000, 0, 0))]).await;

    // Funded exactly the stake → locks exactly the stake, nothing more.
    let exact = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000)]).await;
    accept_ok(&v, &exact).await;
    assert_eq!(v.balances(exact.requester).await, bal(0, 0, STAKE));
    v.assert_conserved().await;
}

// =============================================================================================
// Resolution guards
// =============================================================================================

#[tokio::test]
async fn parlay_resolve_guards() {
    let v = TestVenue::new();
    let p = presented(&v, STAKE, &[("buy_yes", 5_000, 1_000), ("buy_yes", 5_000, 2_000)]).await;
    let oracle = v.oracle_party();

    let (status, body) = v.resolve_leg(oracle, &p.request_id, &p.leg_ids[0], "yes").await;
    assert_eq!(status, StatusCode::CONFLICT, "before Locked: {body}");
    accept_ok(&v, &p).await;

    let ghost = Uuid::new_v4().to_string();
    assert_eq!(v.resolve_leg(oracle, &p.request_id, &ghost, "yes").await.0, StatusCode::NOT_FOUND, "unknown leg");

    resolve_ok(&v, &p, 0, "yes").await;
    let (status, body) = v.resolve_leg(oracle, &p.request_id, &p.leg_ids[0], "no").await;
    assert_eq!(status, StatusCode::CONFLICT, "a leg resolves once: {body}");
    assert_eq!(v.snapshot(&p.request_id).await["legs"][0]["outcome"], "yes", "first answer stands");
    assert_eq!(v.balances(p.requester).await, bal(0, 0, STAKE), "nothing paid out");
    v.assert_conserved().await;
}
