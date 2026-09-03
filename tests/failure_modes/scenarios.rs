//! Scenarios and helpers shared by the state modules. Everything from the HTTP harness is
//! re-exported so each module imports one thing.
#![allow(unused_imports)]

pub use super::common::*;
use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

/// R funded 600, M1 and M2 funded 500. M1 quotes at 50% (locks 500), M2 at 60% (locks 400).
/// R's lock is 500 if M1 wins, 600 if M2 wins.
pub struct OneLeg {
    pub requester: Uuid,
    pub m1: Uuid,
    pub m2: Uuid,
    pub request_id: String,
    pub leg_id: String,
}

pub const M1_PRICE: u32 = 5_000;

pub const M2_PRICE: u32 = 6_000;

pub const M2_LOCK: u64 = 400;

pub async fn one_leg(v: &TestVenue) -> OneLeg {
    let (requester, m1, m2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    v.fund(requester, 600).await;
    v.fund(m1, SIDE_LOCK).await;
    v.fund(m2, SIDE_LOCK).await;
    let (status, created) = v
        .open_request(
            requester,
            json!([leg("buy_yes", LEG_NOTIONAL)]),
            v.at(RESPONSE_DEADLINE_SECS),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    OneLeg {
        requester,
        m1,
        m2,
        request_id: id_of(&created),
        leg_id: leg_ids(&created).remove(0),
    }
}

impl OneLeg {
    pub async fn quote_m1(&self, v: &TestVenue, expires_secs: i64) -> String {
        v.quote_ok(
            self.m1,
            &self.request_id,
            &self.leg_id,
            M1_PRICE,
            LEG_NOTIONAL,
            v.at(expires_secs),
        )
        .await
    }
    pub async fn quote_m2(&self, v: &TestVenue, expires_secs: i64) -> String {
        v.quote_ok(
            self.m2,
            &self.request_id,
            &self.leg_id,
            M2_PRICE,
            LEG_NOTIONAL,
            v.at(expires_secs),
        )
        .await
    }
}

pub fn selections(r: &Value) -> Vec<String> {
    r["package"]["selections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["quote_id"].as_str().unwrap().to_owned())
        .collect()
}

/// Three `buy_yes` legs, Locked, then the oracle reports `yes`. Returns the scenario and the
/// instant the report landed.
pub async fn reported_yes(v: &TestVenue) -> (ThreeLeg, i64) {
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    let reported_at = RESPONSE_DEADLINE_SECS + 1;
    v.set(v.at(reported_at));
    let (status, body) = v.resolve(&s.request_id, "yes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "reported");
    (s, reported_at)
}

pub async fn escrowed_three(v: &TestVenue, s: &ThreeLeg) {
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, SIDE_LOCK));
    }
}
