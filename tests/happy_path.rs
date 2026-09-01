//! End-to-end RFQ lifecycle over the HTTP surface, driven with a mock clock and ledger.
//!
//! Requester → MMs quote → Tick presents → Accept locks escrow → oracle Yes settles.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rfq_matching_settlement_engine::api::{AppState, router};
use rfq_matching_settlement_engine::engine::{Engine, EngineConfig, EngineHandle, spawn_engine};
use rfq_matching_settlement_engine::mocks::{MockClock, MockLedger};

const ACCEPT_WINDOW_SECS: i64 = 60;

struct Venue {
    app: Router,
    engine: EngineHandle,
    clock: Arc<MockClock>,
    ledger: Arc<MockLedger>,
    t0: DateTime<Utc>,
}

impl Venue {
    fn start() -> Self {
        let t0 = DateTime::from_timestamp(1_756_728_000, 0).unwrap();
        let clock = Arc::new(MockClock::new(t0));
        let ledger = Arc::new(MockLedger::new());
        let engine = Engine::new(
            ledger.clone(),
            clock.clone(),
            EngineConfig { accept_window: Duration::seconds(ACCEPT_WINDOW_SECS) },
        );
        let (engine, _actor) = spawn_engine(engine);
        let app = router(AppState { engine: engine.clone() });
        Self { app, engine, clock, ledger, t0 }
    }

    fn at(&self, secs: i64) -> DateTime<Utc> {
        self.t0 + Duration::seconds(secs)
    }

    /// Move the mock clock and tick the engine, as the expiry worker would.
    async fn advance_to(&self, secs: i64) {
        let now = self.at(secs);
        self.clock.set(now);
        self.engine.tick(now).await.unwrap();
    }

    async fn call(&self, method: Method, path: &str, party: Option<Uuid>, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(p) = party {
            builder = builder.header("x-party-id", p.to_string());
        }
        let request = match body {
            Some(v) => builder.header(CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
        };
        (status, json)
    }

    async fn credit(&self, party: Uuid, amount: u64) {
        let (status, body) = self
            .call(Method::POST, "/v1/ledger/credit", None, Some(json!({ "party_id": party, "amount": amount })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn balance(&self, party: Uuid) -> (u64, u64, u64) {
        let (status, body) = self.call(Method::GET, &format!("/v1/ledger/{party}"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (
            body["free"].as_u64().unwrap(),
            body["reserved"].as_u64().unwrap(),
            body["escrowed"].as_u64().unwrap(),
        )
    }

    async fn get_request(&self, id: &str) -> Value {
        let (status, body) = self.call(Method::GET, &format!("/v1/requests/{id}"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn quote(&self, maker: Uuid, request_id: &str, leg_id: &str, price_bps: u32, size: u64, expires_at: DateTime<Utc>) -> String {
        let (status, body) = self
            .call(
                Method::POST,
                &format!("/v1/requests/{request_id}/quotes"),
                Some(maker),
                Some(json!({ "leg_id": leg_id, "price_bps": price_bps, "size": size, "expires_at": expires_at })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["state"], "live");
        body["id"].as_str().unwrap().to_owned()
    }
}

fn quote_state<'a>(request: &'a Value, quote_id: &str) -> &'a str {
    request["quotes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"] == quote_id)
        .unwrap_or_else(|| panic!("quote {quote_id} missing"))["state"]
        .as_str()
        .unwrap()
}

#[tokio::test]
async fn full_lifecycle_two_legs_settles_yes() {
    let v = Venue::start();
    let requester = Uuid::new_v4();
    let mm1 = Uuid::new_v4();
    let mm2 = Uuid::new_v4();

    // ---- Faucet -----------------------------------------------------------------------
    for p in [requester, mm1, mm2] {
        v.credit(p, 10_000).await;
        assert_eq!(v.balance(p).await, (10_000, 0, 0));
    }

    // ---- Requester opens a two-leg RFQ ----------------------------------------------------
    // Leg A: requester buys Yes on "A", notional 1_000.
    // Leg B: requester sells Yes on "B", notional 2_000.
    let response_deadline = v.at(30);
    let (status, req) = v
        .call(
            Method::POST,
            "/v1/requests",
            Some(requester),
            Some(json!({
                "legs": [
                    { "contract": "A", "description": "A resolves Yes by 2026-12-31", "side": "buy_yes",  "notional": 1_000 },
                    { "contract": "B", "description": "B resolves Yes by 2026-12-31", "side": "sell_yes", "notional": 2_000 }
                ],
                "response_deadline": response_deadline
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{req}");
    assert_eq!(req["state"], "open");
    assert_eq!(req["requester"], requester.to_string());
    assert_eq!(req["legs"][0]["description"], "A resolves Yes by 2026-12-31");
    assert!(req.get("package").is_none());
    let request_id = req["id"].as_str().unwrap().to_owned();
    let leg_a = req["legs"][0]["id"].as_str().unwrap().to_owned();
    let leg_b = req["legs"][1]["id"].as_str().unwrap().to_owned();

    // Requester's free balance is untouched while Open: the price is not known yet.
    assert_eq!(v.balance(requester).await, (10_000, 0, 0));

    // ---- Market makers quote; collateral is reserved at submit ---------------------------
    let expires = v.at(600);
    // Leg A (BuyYes): MM is the Yes-seller and reserves (1 - p) * n.
    let q_a_mm1 = v.quote(mm1, &request_id, &leg_a, 4_000, 1_000, expires).await; // 600
    assert_eq!(v.balance(mm1).await, (9_400, 600, 0));
    let q_a_mm2 = v.quote(mm2, &request_id, &leg_a, 3_500, 1_000, expires).await; // 650, better for a Yes-buyer
    assert_eq!(v.balance(mm2).await, (9_350, 650, 0));
    // Leg B (SellYes): MM is the Yes-buyer and reserves p * n.
    let q_b_mm1 = v.quote(mm1, &request_id, &leg_b, 6_000, 2_000, expires).await; // 1_200
    assert_eq!(v.balance(mm1).await, (8_200, 1_800, 0));
    let q_b_mm2 = v.quote(mm2, &request_id, &leg_b, 6_500, 2_500, expires).await; // 1_300, better for a Yes-seller
    assert_eq!(v.balance(mm2).await, (8_050, 1_950, 0));

    // mm1 posts a would-be-winning quote on leg A, then cancels it: the reservation comes back.
    let q_a_mm1_cheap = v.quote(mm1, &request_id, &leg_a, 3_000, 1_000, expires).await; // 700
    assert_eq!(v.balance(mm1).await, (7_500, 2_500, 0));
    let (status, body) = v.call(Method::DELETE, &format!("/v1/quotes/{q_a_mm1_cheap}"), Some(mm1), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(v.balance(mm1).await, (8_200, 1_800, 0));

    let snapshot = v.get_request(&request_id).await;
    assert_eq!(snapshot["state"], "open");
    assert_eq!(quote_state(&snapshot, &q_a_mm1_cheap), "released");
    assert_eq!(snapshot["quotes"].as_array().unwrap().len(), 5);

    // ---- Response deadline passes: the worker presents the best package ------------------
    v.advance_to(30).await;
    let presented = v.get_request(&request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(presented["accept_deadline"], json!(v.at(30 + ACCEPT_WINDOW_SECS)));
    let selections = presented["package"]["selections"].as_array().unwrap();
    assert_eq!(selections.len(), 2);
    assert_eq!(selections[0], json!({ "leg_id": leg_a, "quote_id": q_a_mm2 }));
    assert_eq!(selections[1], json!({ "leg_id": leg_b, "quote_id": q_b_mm2 }));
    assert_eq!(quote_state(&presented, &q_a_mm2), "selected");
    assert_eq!(quote_state(&presented, &q_b_mm2), "selected");
    assert_eq!(quote_state(&presented, &q_a_mm1), "live");
    assert_eq!(quote_state(&presented, &q_b_mm1), "live");
    // Nothing has moved to escrow yet; losers stay reserved until accept.
    assert_eq!(v.balance(mm1).await, (8_200, 1_800, 0));
    assert_eq!(v.balance(mm2).await, (8_050, 1_950, 0));
    assert_eq!(v.balance(requester).await, (10_000, 0, 0));

    // ---- Requester accepts: one lock_batch, losers released ------------------------------
    let (status, locked) = v.call(Method::POST, &format!("/v1/requests/{request_id}/accept"), Some(requester), None).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["state"], "locked");
    let escrows = locked["escrows"].as_array().unwrap();
    assert_eq!(escrows.len(), 2);
    // Leg A @ 35%: requester (Yes-buyer) 350, mm2 (Yes-seller) 650.
    assert_eq!(
        escrows[0],
        json!({
            "leg_id": leg_a, "yes_buyer": requester, "yes_seller": mm2,
            "yes_buyer_amount": 350, "yes_seller_amount": 650, "notional": 1_000
        })
    );
    // Leg B @ 65%: mm2 (Yes-buyer) 1_300, requester (Yes-seller) 700.
    assert_eq!(
        escrows[1],
        json!({
            "leg_id": leg_b, "yes_buyer": mm2, "yes_seller": requester,
            "yes_buyer_amount": 1_300, "yes_seller_amount": 700, "notional": 2_000
        })
    );
    assert_eq!(quote_state(&locked, &q_a_mm2), "locked");
    assert_eq!(quote_state(&locked, &q_b_mm2), "locked");
    assert_eq!(quote_state(&locked, &q_a_mm1), "released");
    assert_eq!(quote_state(&locked, &q_b_mm1), "released");

    assert_eq!(v.balance(requester).await, (8_950, 0, 1_050));
    assert_eq!(v.balance(mm2).await, (8_050, 0, 1_950));
    assert_eq!(v.balance(mm1).await, (10_000, 0, 0));
    assert!(v.ledger.conservation_holds());

    // ---- Oracle says Yes: each leg's Yes-buyer receives its notional ---------------------
    let (status, settled) = v
        .call(Method::POST, "/v1/oracle/resolve", None, Some(json!({ "request_id": request_id, "outcome": "yes" })))
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");

    assert_eq!(v.balance(requester).await, (9_950, 0, 0)); // 8_950 + 1_000 (leg A)
    assert_eq!(v.balance(mm2).await, (10_050, 0, 0)); // 8_050 + 2_000 (leg B)
    assert_eq!(v.balance(mm1).await, (10_000, 0, 0));
    assert!(v.ledger.conservation_holds());

    // ---- Terminal: a second accept or resolve is a 409 -----------------------------------
    let (status, body) = v.call(Method::POST, &format!("/v1/requests/{request_id}/accept"), Some(requester), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    let (status, _) = v
        .call(Method::POST, "/v1/oracle/resolve", None, Some(json!({ "request_id": request_id, "outcome": "no" })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v.get_request(&request_id).await["state"], "settled");
}

#[tokio::test]
async fn unmatched_leg_fails_request_and_releases_every_reservation() {
    let v = Venue::start();
    let requester = Uuid::new_v4();
    let mm = Uuid::new_v4();
    v.credit(requester, 5_000).await;
    v.credit(mm, 5_000).await;

    let (status, req) = v
        .call(
            Method::POST,
            "/v1/requests",
            Some(requester),
            Some(json!({
                "legs": [
                    { "contract": "A", "description": "A", "side": "buy_yes", "notional": 1_000 },
                    { "contract": "B", "description": "B", "side": "buy_yes", "notional": 1_000 },
                    { "contract": "C", "description": "C", "side": "buy_yes", "notional": 1_000 }
                ],
                "response_deadline": v.at(30)
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{req}");
    let request_id = req["id"].as_str().unwrap().to_owned();
    let leg_a = req["legs"][0]["id"].as_str().unwrap().to_owned();
    let leg_b = req["legs"][1]["id"].as_str().unwrap().to_owned();
    let leg_c = req["legs"][2]["id"].as_str().unwrap().to_owned();

    // Quotes on legs 1 and 3 only.
    v.quote(mm, &request_id, &leg_a, 5_000, 1_000, v.at(600)).await;
    v.quote(mm, &request_id, &leg_c, 5_000, 1_000, v.at(600)).await;
    assert_eq!(v.balance(mm).await, (4_000, 1_000, 0));

    v.advance_to(30).await;
    let failed = v.get_request(&request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["fail_reason"], json!({ "reason": "leg_unmatched", "leg_id": leg_b }));
    assert!(failed.get("package").is_none(), "the requester is never shown a partial package");
    assert!(failed["quotes"].as_array().unwrap().iter().all(|q| q["state"] == "released"));
    assert_eq!(v.balance(mm).await, (5_000, 0, 0));
    assert_eq!(v.balance(requester).await, (5_000, 0, 0));
    assert!(v.ledger.conservation_holds());
}

#[tokio::test]
async fn missing_party_header_is_unauthorized() {
    let v = Venue::start();
    let (status, body) = v
        .call(Method::POST, "/v1/requests", None, Some(json!({ "legs": [], "response_deadline": v.at(30) })))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "missing_party");
}
