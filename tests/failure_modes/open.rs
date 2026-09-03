//! Open: admission and quoting. Every refusal here stores nothing and reserves nothing.

use axum::http::Method;
use axum::http::StatusCode;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::scenarios::*;

#[tokio::test]
async fn fm_missing_party_header_is_401() {
    let v = TestVenue::new();
    let body = json!({ "legs": [leg("buy_yes", LEG_NOTIONAL)], "tenor": "five_minutes", "response_deadline": ts(v.at(RESPONSE_DEADLINE_SECS)) });
    let (status, json) = v.call(Method::POST, "/v1/requests", None, Some(body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
    assert_eq!(json["code"], "missing_party");
    let (status, _) = v
        .call(
            Method::POST,
            "/v1/requests/x/accept",
            Some(Uuid::new_v4()),
            None,
        )
        .await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "header present → past the extractor"
    );
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_unknown_ids_are_404() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let ghost = Uuid::new_v4().to_string();
    assert_eq!(v.get_request(&ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.accept(s.requester, &ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        v.quote(
            s.m1,
            &ghost,
            &s.leg_id,
            M1_PRICE,
            LEG_NOTIONAL,
            v.at(QUOTE_EXPIRY_SECS)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        v.quote(
            s.m1,
            &s.request_id,
            &ghost,
            M1_PRICE,
            LEG_NOTIONAL,
            v.at(QUOTE_EXPIRY_SECS)
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "unknown leg"
    );
    assert_eq!(v.cancel_quote(s.m1, &ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.resolve(&ghost, "yes").await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.balances(s.m1).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_invalid_body_rejected() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let deadline = v.at(RESPONSE_DEADLINE_SECS);

    for (price, label) in [(0, "0%"), (10_000, "100%")] {
        let (status, body) = v
            .quote(
                s.m1,
                &s.request_id,
                &s.leg_id,
                price,
                LEG_NOTIONAL,
                v.at(QUOTE_EXPIRY_SECS),
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_price")),
            "price {label}"
        );
    }
    let (status, body) = v
        .open_request(s.requester, json!([leg("buy_yes", 0)]), deadline)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("zero_notional"))
    );
    let (status, body) = v.open_request(s.requester, json!([]), deadline).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("empty_legs"))
    );
    let mut blank = leg("buy_yes", LEG_NOTIONAL);
    blank["contract"] = json!("   ");
    let (status, body) = v.open_request(s.requester, json!([blank]), deadline).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_contract_id"))
    );

    assert_eq!(v.balances(s.m1).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_response_deadline_in_past_rejected() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    for (deadline, label) in [(v.at(-1), "past"), (v.now(), "now")] {
        let (status, body) = v
            .open_request(requester, json!([leg("buy_yes", LEG_NOTIONAL)]), deadline)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_eq!(body["code"], "deadline_in_past");
    }
    v.assert_conserved().await;
}

/// REVIEW #1. A response deadline centuries out is not a request the venue can honour: it
/// must be refused at the door, with nothing stored.
#[tokio::test]
async fn fm_response_deadline_beyond_horizon_rejected() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    let two_hundred_years_out = v.at(200 * 365 * 24 * 3_600);
    let (status, body) = v
        .open_request(
            requester,
            json!([leg("buy_yes", LEG_NOTIONAL)]),
            two_hundred_years_out,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a deadline centuries out must be rejected: {body}"
    );
    assert_eq!(body["code"], "deadline_beyond_horizon");
    assert!(body.get("id").is_none(), "nothing stored: {body}");
    v.assert_conserved().await;
}

/// REVIEW #1. A deadline near the end of representable time is refused at the door, and the
/// venue keeps serving everyone else afterwards.
#[tokio::test]
async fn fm_far_future_deadline_cannot_kill_engine() {
    let v = TestVenue::new();
    let (requester, maker) = (Uuid::new_v4(), Uuid::new_v4());
    v.fund(requester, SIDE_LOCK).await;
    v.fund(maker, SIDE_LOCK).await;

    // Less headroom below MAX_UTC than one accept window.
    let near_max = DateTime::<Utc>::MAX_UTC - Duration::seconds(10);
    let (status, body) = v
        .open_request(requester, json!([leg("buy_yes", LEG_NOTIONAL)]), near_max)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "deadline_beyond_horizon");
    assert!(body.get("id").is_none(), "nothing stored: {body}");

    // Venue still answers: an unrelated party can credit and open a request.
    let bystander = Uuid::new_v4();
    let (status, body) = v.credit(bystander, SIDE_LOCK).await;
    assert_eq!(status, StatusCode::OK, "venue stopped answering: {body}");
    let (status, body) = v
        .open_request(
            bystander,
            json!([leg("buy_yes", LEG_NOTIONAL)]),
            v.at(RESPONSE_DEADLINE_SECS),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "venue stopped answering: {body}"
    );
    assert_eq!(
        v.balances(requester).await,
        bal(SIDE_LOCK, 0, 0),
        "nothing reserved"
    );
    assert_eq!(v.balances(maker).await, bal(SIDE_LOCK, 0, 0));
    assert_eq!(v.balances(bystander).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_maker_insufficient_funds_at_quote_is_402() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let poor = Uuid::new_v4();
    v.fund(poor, SIDE_LOCK - 1).await;

    let (status, body) = v
        .quote(
            poor,
            &s.request_id,
            &s.leg_id,
            M1_PRICE,
            LEG_NOTIONAL,
            v.at(QUOTE_EXPIRY_SECS),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["code"], "insufficient_funds");
    assert_eq!(v.snapshot(&s.request_id).await["quotes"], json!([]));
    assert_eq!(v.balances(poor).await, bal(SIDE_LOCK - 1, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_quote_too_small_rejected() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;

    let (status, body) = v
        .quote(
            s.makers[0],
            &s.request_id,
            &s.leg_ids[0],
            LEG_PRICE_BPS,
            LEG_NOTIONAL - 1,
            v.at(QUOTE_EXPIRY_SECS),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_too_small");
    assert_eq!(v.balances(s.makers[0]).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_quote_expiring_before_accept_window_rejected_at_submit() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let m = s.makers[0];

    let (status, body) = v
        .quote(
            m,
            &s.request_id,
            &s.leg_ids[0],
            LEG_PRICE_BPS,
            LEG_NOTIONAL,
            v.at(ACCEPT_DEADLINE_SECS - 1),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_expires_before_accept_window");

    let (status, body) = v
        .quote(
            m,
            &s.request_id,
            &s.leg_ids[0],
            LEG_PRICE_BPS,
            LEG_NOTIONAL,
            v.now(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_expired");

    assert_eq!(v.snapshot(&s.request_id).await["quotes"], json!([]));
    assert_eq!(
        v.balances(m).await,
        bal(SIDE_LOCK, 0, 0),
        "nothing reserved"
    );
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_maker_cannot_cancel_others_quote() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let q0 = v.quote_leg(&s, 0).await;

    let (status, body) = v.cancel_quote(s.makers[1], &q0).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "not_owner");
    assert_quote_states(&v.snapshot(&s.request_id).await, &[(&q0, "live")]);
    assert_eq!(v.balances(s.makers[0]).await, bal(0, SIDE_LOCK, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_cancel_released_quote_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let q0 = v.quote_leg(&s, 0).await;
    assert_eq!(
        v.cancel_quote(s.makers[0], &q0).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(v.balances(s.makers[0]).await, bal(SIDE_LOCK, 0, 0));

    let (status, body) = v.cancel_quote(s.makers[0], &q0).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "quote_not_live");
    assert_eq!(
        v.balances(s.makers[0]).await,
        bal(SIDE_LOCK, 0, 0),
        "no double release"
    );
    v.assert_conserved().await;
}
