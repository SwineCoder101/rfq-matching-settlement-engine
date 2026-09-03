//! End-to-end RFQ lifecycle over the HTTP surface, driven with a mock clock and ledger.
//!
//! Requester → MMs quote → Tick presents → Accept locks escrow → oracle Yes settles.
//! Request/response payloads live in `tests/fixtures`; the harness is in `tests/common`.

mod common;

use axum::http::{Method, StatusCode};
use common::{
    ACCEPT_WINDOW_SECS, TestVenue, assert_quote_states, bal, fixture, id_of, leg_ids, ts,
};
use uuid::Uuid;

#[tokio::test]
async fn full_lifecycle_two_legs_settles_yes() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    let mm1 = Uuid::new_v4();
    let mm2 = Uuid::new_v4();

    // ---- Faucet -----------------------------------------------------------------------
    for p in [requester, mm1, mm2] {
        v.fund(p, 10_000).await;
    }
    v.assert_balances(&[
        ("requester", requester, bal(10_000, 0, 0)),
        ("mm1", mm1, bal(10_000, 0, 0)),
        ("mm2", mm2, bal(10_000, 0, 0)),
    ])
    .await;

    // ---- Requester opens a two-leg RFQ ----------------------------------------------------
    // Leg A: requester buys Yes on "A", notional 1_000.  Leg B: requester sells Yes on "B", 2_000.
    let response_deadline = v.at(30);
    let created = v
        .create_request(requester, "requests/rfq_two_leg.json", response_deadline)
        .await;
    let request_id = id_of(&created);
    let [leg_a, leg_b] = <[String; 2]>::try_from(leg_ids(&created)).unwrap();
    assert_eq!(
        created,
        fixture(
            "responses/rfq_two_leg_open.json",
            vars![
                "id" => request_id, "requester" => requester, "leg_a" => leg_a, "leg_b" => leg_b,
                "response_deadline" => ts(response_deadline), "created_at" => ts(v.now()),
            ],
        )
    );
    // Requester's free balance is untouched while Open: the price is not known yet.
    assert_eq!(v.balances(requester).await, bal(10_000, 0, 0));

    // ---- Market makers quote; collateral is reserved at submit ---------------------------
    let expires = v.at(600);
    // Leg A (BuyYes): MM is the Yes-seller and reserves (1 - p) * n.
    let q_a_mm1 = v
        .quote_ok(mm1, &request_id, &leg_a, 4_000, 1_000, expires)
        .await; // 600
    assert_eq!(v.balances(mm1).await, bal(9_400, 600, 0));
    let q_a_mm2 = v
        .quote_ok(mm2, &request_id, &leg_a, 3_500, 1_000, expires)
        .await; // 650, better for a Yes-buyer
    assert_eq!(v.balances(mm2).await, bal(9_350, 650, 0));
    // Leg B (SellYes): MM is the Yes-buyer and reserves p * n.
    let q_b_mm1 = v
        .quote_ok(mm1, &request_id, &leg_b, 6_000, 2_000, expires)
        .await; // 1_200
    assert_eq!(v.balances(mm1).await, bal(8_200, 1_800, 0));
    let q_b_mm2 = v
        .quote_ok(mm2, &request_id, &leg_b, 6_500, 2_500, expires)
        .await; // 1_300, better for a Yes-seller
    assert_eq!(v.balances(mm2).await, bal(8_050, 1_950, 0));

    // mm1 posts a would-be-winning quote on leg A, then cancels it: the reservation comes back.
    let q_a_mm1_cheap = v
        .quote_ok(mm1, &request_id, &leg_a, 3_000, 1_000, expires)
        .await; // 700
    assert_eq!(v.balances(mm1).await, bal(7_500, 2_500, 0));
    assert_eq!(
        v.cancel_quote(mm1, &q_a_mm1_cheap).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(v.balances(mm1).await, bal(8_200, 1_800, 0));

    let open = v.snapshot(&request_id).await;
    assert_eq!(open["state"], "open");
    assert_eq!(open["quotes"].as_array().unwrap().len(), 5);
    assert_quote_states(&open, &[(&q_a_mm1_cheap, "released")]);

    // ---- Response deadline passes: the worker presents the best package ------------------
    v.advance_to(30).await;
    let presented = v.snapshot(&request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(
        presented["accept_deadline"],
        ts(v.at(30 + ACCEPT_WINDOW_SECS))
    );
    assert_eq!(
        presented["package"],
        fixture(
            "responses/package_two_leg.json",
            vars!["leg_a" => leg_a, "quote_a" => q_a_mm2, "leg_b" => leg_b, "quote_b" => q_b_mm2],
        )
    );
    assert_quote_states(
        &presented,
        &[
            (&q_a_mm2, "selected"),
            (&q_b_mm2, "selected"),
            (&q_a_mm1, "live"),
            (&q_b_mm1, "live"),
        ],
    );
    // Nothing has moved to escrow yet; losers stay reserved until accept.
    v.assert_balances(&[
        ("requester", requester, bal(10_000, 0, 0)),
        ("mm1", mm1, bal(8_200, 1_800, 0)),
        ("mm2", mm2, bal(8_050, 1_950, 0)),
    ])
    .await;

    // ---- Requester accepts: one lock_batch, losers released ------------------------------
    let (status, locked) = v.accept(requester, &request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["state"], "locked");
    assert_eq!(
        locked["escrows"],
        fixture(
            "responses/escrows_two_leg_locked.json",
            vars!["leg_a" => leg_a, "leg_b" => leg_b, "requester" => requester, "maker" => mm2],
        )
    );
    assert_quote_states(
        &locked,
        &[
            (&q_a_mm2, "locked"),
            (&q_b_mm2, "locked"),
            (&q_a_mm1, "released"),
            (&q_b_mm1, "released"),
        ],
    );
    v.assert_balances(&[
        ("requester", requester, bal(8_950, 0, 1_050)), // 350 (leg A) + 700 (leg B)
        ("mm1", mm1, bal(10_000, 0, 0)),
        ("mm2", mm2, bal(8_050, 0, 1_950)), // 650 (leg A) + 1_300 (leg B)
    ])
    .await;

    // ---- Oracle says Yes: each leg's Yes-buyer receives its notional ---------------------
    let (status, settled) = v.resolve(&request_id, "yes").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    v.assert_balances(&[
        ("requester", requester, bal(9_950, 0, 0)), // 8_950 + 1_000 (leg A)
        ("mm1", mm1, bal(10_000, 0, 0)),
        ("mm2", mm2, bal(10_050, 0, 0)), // 8_050 + 2_000 (leg B)
    ])
    .await;

    // ---- Terminal: a second accept or resolve is a 409 -----------------------------------
    let (status, body) = v.accept(requester, &request_id).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        fixture(
            "responses/error_wrong_state_settled.json",
            vars!["expected" => "Presented"]
        )
    );
    let (status, body) = v.resolve(&request_id, "no").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        fixture(
            "responses/error_wrong_state_settled.json",
            vars!["expected" => "Locked"]
        )
    );
    assert_eq!(v.snapshot(&request_id).await["state"], "settled");
    v.assert_conserved().await;
}

#[tokio::test]
async fn unmatched_leg_fails_request_and_releases_every_reservation() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    let mm = Uuid::new_v4();
    v.fund(requester, 5_000).await;
    v.fund(mm, 5_000).await;

    let created = v
        .create_request(requester, "requests/rfq_three_leg.json", v.at(30))
        .await;
    let request_id = id_of(&created);
    let [leg_a, leg_b, leg_c] = <[String; 3]>::try_from(leg_ids(&created)).unwrap();

    // Quotes on legs 1 and 3 only.
    v.quote_ok(mm, &request_id, &leg_a, 5_000, 1_000, v.at(600))
        .await;
    v.quote_ok(mm, &request_id, &leg_c, 5_000, 1_000, v.at(600))
        .await;
    assert_eq!(v.balances(mm).await, bal(4_000, 1_000, 0));

    v.advance_to(30).await;
    let failed = v.snapshot(&request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["fail_reason"],
        fixture(
            "responses/fail_reason_leg_unmatched.json",
            vars!["leg_id" => leg_b]
        )
    );
    assert!(
        failed.get("package").is_none(),
        "the requester is never shown a partial package"
    );
    assert!(
        failed["quotes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["state"] == "released")
    );
    v.assert_balances(&[
        ("requester", requester, bal(5_000, 0, 0)),
        ("mm", mm, bal(5_000, 0, 0)),
    ])
    .await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn missing_party_header_is_unauthorized() {
    let v = TestVenue::new();
    let body = fixture(
        "requests/rfq_two_leg.json",
        vars!["response_deadline" => ts(v.at(30))],
    );
    let (status, json) = v.call(Method::POST, "/v1/requests", None, Some(body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
    assert_eq!(json, fixture("responses/error_missing_party.json", vec![]));
}
