//! Deterministic harness for HTTP-level tests.
//!
//! `TestVenue` runs the Axum app in-process over the real engine actor, an in-memory ledger,
//! and a `MockClock` the test controls. The expiry worker is **not** started: tests move time
//! with `advance` / `set` and deliver `Tick` explicitly with `tick()`, so every race is
//! reproducible. No sleeps, no wall clock.
#![allow(dead_code)] // each test binary uses a different subset of the harness

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rfq_matching_settlement_engine::api::{AppState, PARTY_HEADER, router};
use rfq_matching_settlement_engine::clock::Clock;
use rfq_matching_settlement_engine::domain::PartyId;
use rfq_matching_settlement_engine::engine::{Engine, EngineConfig, EngineHandle, spawn_engine};
use rfq_matching_settlement_engine::mock::{MockClock, MockLedger};

pub const ACCEPT_WINDOW_SECS: i64 = 60;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// Build the substitution list for [`fixture`]: `vars!["id" => some_uuid, "amount" => 5]`.
#[macro_export]
macro_rules! vars {
    ($($key:literal => $value:expr),* $(,)?) => {
        vec![$(($key, $value.to_string())),*]
    };
}

/// Load `tests/fixtures/<name>`, replace every `{{key}}` with its value, and parse as JSON.
/// Substitution happens on the raw text so placeholders work for strings *and* numbers.
pub fn fixture(name: &str, vars: Vec<(&str, String)>) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let mut text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    for (key, value) in vars {
        text = text.replace(&format!("{{{{{key}}}}}"), &value);
    }
    assert!(
        !text.contains("{{"),
        "fixture {name} has unsubstituted placeholders:\n{text}"
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}\n{text}"))
}

/// A timestamp exactly as the server serializes it.
pub fn ts(t: DateTime<Utc>) -> String {
    serde_json::to_value(t)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

/// One leg for `open_request`.
pub fn leg(side: &str, notional: u64) -> Value {
    json!({ "contract": format!("{side}-{notional}"), "description": format!("Settles Yes if index {side}-{notional} closes above 100.00 per the venue's published source at 2026-12-31T00:00:00Z; otherwise No."), "side": side, "notional": notional })
}

// ---------------------------------------------------------------------------------------------
// Balances
// ---------------------------------------------------------------------------------------------

/// Free / reserved / escrowed, in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balances {
    pub free: u64,
    pub reserved: u64,
    pub escrowed: u64,
}

pub const fn bal(free: u64, reserved: u64, escrowed: u64) -> Balances {
    Balances {
        free,
        reserved,
        escrowed,
    }
}

// ---------------------------------------------------------------------------------------------
// Venue
// ---------------------------------------------------------------------------------------------

pub struct TestVenue {
    app: Router,
    engine: EngineHandle,
    clock: Arc<MockClock>,
    pub ledger: Arc<MockLedger>,
    t0: DateTime<Utc>,
    /// Every request id this venue has opened, for `assert_conserved`.
    requests: Mutex<Vec<String>>,
}

impl TestVenue {
    pub fn new() -> Self {
        let t0 = DateTime::from_timestamp(1_756_728_000, 0).unwrap();
        let clock = Arc::new(MockClock::new(t0));
        let ledger = Arc::new(MockLedger::new());
        let engine = Engine::new(
            ledger.clone(),
            clock.clone(),
            EngineConfig {
                accept_window: Duration::seconds(ACCEPT_WINDOW_SECS),
                ..EngineConfig::default()
            },
        );
        let (engine, _actor) = spawn_engine(engine);
        let app = router(AppState {
            engine: engine.clone(),
        });
        Self {
            app,
            engine,
            clock,
            ledger,
            t0,
            requests: Mutex::new(Vec::new()),
        }
    }

    // ---- time ------------------------------------------------------------------------------

    /// `t0 + secs`.
    pub fn at(&self, secs: i64) -> DateTime<Utc> {
        self.t0 + Duration::seconds(secs)
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    pub fn set(&self, now: DateTime<Utc>) {
        self.clock.set(now);
    }

    /// Deliver `Tick { now: clock.now() }` and wait until the engine has processed it.
    pub async fn tick(&self) {
        self.tick_at(self.now()).await;
    }

    /// Deliver `Tick { now }` with an explicit timestamp (the worker's view of time may differ
    /// from the clock at the instant another command lands) and wait for processing.
    pub async fn tick_at(&self, now: DateTime<Utc>) {
        self.engine.tick(now).await.unwrap();
        // Commands are processed in order; a read behind the tick is a barrier.
        self.engine
            .balance(PartyId::from(Uuid::nil()))
            .await
            .unwrap();
    }

    /// Move the clock to `t0 + secs` and tick.
    pub async fn advance_to(&self, secs: i64) {
        self.set(self.at(secs));
        self.tick().await;
    }

    // ---- raw HTTP --------------------------------------------------------------------------

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        party: Option<Uuid>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(p) = party {
            builder = builder.header(PARTY_HEADER, p.to_string());
        }
        let request = match body {
            Some(v) => builder
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
        };
        (status, body)
    }

    // ---- client: every call returns (status, body) -----------------------------------------

    pub async fn credit(&self, party: Uuid, amount: u64) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            "/v1/ledger/credit",
            None,
            Some(json!({ "party_id": party, "amount": amount })),
        )
        .await
    }

    pub async fn balance(&self, party: Uuid) -> (StatusCode, Value) {
        self.call(Method::GET, &format!("/v1/ledger/{party}"), None, None)
            .await
    }

    pub async fn open_request(
        &self,
        requester: Uuid,
        legs: Value,
        response_deadline: DateTime<Utc>,
    ) -> (StatusCode, Value) {
        let body = json!({ "legs": legs, "response_deadline": ts(response_deadline) });
        let (status, json) = self
            .call(Method::POST, "/v1/requests", Some(requester), Some(body))
            .await;
        if status == StatusCode::CREATED {
            self.requests.lock().unwrap().push(id_of(&json));
        }
        (status, json)
    }

    pub async fn get_request(&self, id: &str) -> (StatusCode, Value) {
        self.call(Method::GET, &format!("/v1/requests/{id}"), None, None)
            .await
    }

    pub async fn quote(
        &self,
        maker: Uuid,
        request_id: &str,
        leg_id: &str,
        price_bps: u32,
        size: u64,
        expires_at: DateTime<Utc>,
    ) -> (StatusCode, Value) {
        let body = json!({ "leg_id": leg_id, "price_bps": price_bps, "size": size, "expires_at": ts(expires_at) });
        self.call(
            Method::POST,
            &format!("/v1/requests/{request_id}/quotes"),
            Some(maker),
            Some(body),
        )
        .await
    }

    pub async fn cancel_quote(&self, maker: Uuid, quote_id: &str) -> (StatusCode, Value) {
        self.call(
            Method::DELETE,
            &format!("/v1/quotes/{quote_id}"),
            Some(maker),
            None,
        )
        .await
    }

    pub async fn accept(&self, party: Uuid, request_id: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &format!("/v1/requests/{request_id}/accept"),
            Some(party),
            None,
        )
        .await
    }

    pub async fn reject(&self, party: Uuid, request_id: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &format!("/v1/requests/{request_id}/reject"),
            Some(party),
            None,
        )
        .await
    }

    pub async fn resolve(&self, request_id: &str, outcome: &str) -> (StatusCode, Value) {
        let body = json!({ "request_id": request_id, "outcome": outcome });
        self.call(Method::POST, "/v1/oracle/resolve", None, Some(body))
            .await
    }

    // ---- checked conveniences (assert the status, return the useful part) ------------------

    pub async fn fund(&self, party: Uuid, amount: u64) {
        let (status, body) = self.credit(party, amount).await;
        assert_eq!(status, StatusCode::OK, "credit {party}: {body}");
    }

    pub async fn balances(&self, party: Uuid) -> Balances {
        let (status, json) = self.balance(party).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        Balances {
            free: json["free"].as_u64().unwrap(),
            reserved: json["reserved"].as_u64().unwrap(),
            escrowed: json["escrowed"].as_u64().unwrap(),
        }
    }

    /// Assert several parties' balances at once, and that the ledger still conserves funds.
    pub async fn assert_balances(&self, expected: &[(&str, Uuid, Balances)]) {
        for (label, party, want) in expected {
            let got = self.balances(*party).await;
            assert_eq!(got, *want, "balances of {label}");
        }
        assert!(
            self.ledger.conservation_holds(),
            "ledger conservation violated"
        );
    }

    /// `POST /v1/requests` from a fixture; asserts 201 and returns the body.
    pub async fn create_request(
        &self,
        requester: Uuid,
        fixture_name: &str,
        response_deadline: DateTime<Utc>,
    ) -> Value {
        let body = fixture(
            fixture_name,
            vars!["response_deadline" => ts(response_deadline)],
        );
        self.create_request_body(requester, body).await
    }

    /// `POST /v1/requests` with an arbitrary body; asserts 201 and returns the body.
    pub async fn create_request_body(&self, requester: Uuid, body: Value) -> Value {
        let (status, json) = self
            .call(Method::POST, "/v1/requests", Some(requester), Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "POST /v1/requests → {json}");
        self.requests.lock().unwrap().push(id_of(&json));
        json
    }

    /// `GET /v1/requests/{id}`; asserts 200.
    pub async fn snapshot(&self, id: &str) -> Value {
        let (status, json) = self.get_request(id).await;
        assert_eq!(status, StatusCode::OK, "GET /v1/requests/{id} → {json}");
        json
    }

    /// Submit a quote; asserts 201 and returns the quote id.
    pub async fn quote_ok(
        &self,
        maker: Uuid,
        request_id: &str,
        leg_id: &str,
        price_bps: u32,
        size: u64,
        expires_at: DateTime<Utc>,
    ) -> String {
        let (status, json) = self
            .quote(maker, request_id, leg_id, price_bps, size, expires_at)
            .await;
        assert_eq!(status, StatusCode::CREATED, "quote → {json}");
        assert_eq!(json["state"], "live");
        id_of(&json)
    }

    // ---- conservation ----------------------------------------------------------------------

    /// For every party ever credited: `free + reserved + escrowed == credited − paid to others
    /// + received from others`. And the venue's total escrow equals the sum of notionals on
    /// `Locked` / `Disputed` requests. Call at the end of every test.
    pub async fn assert_conserved(&self) {
        for a in self.ledger.audit() {
            let expected = i128::from(a.credited.minor_units())
                - i128::from(a.paid_to_others.minor_units())
                + i128::from(a.received_from_others.minor_units());
            assert_eq!(
                i128::from(a.account.total().minor_units()),
                expected,
                "party {} holds {:?} but credited {} − paid out {} + received {}",
                a.party,
                a.account,
                a.credited,
                a.paid_to_others,
                a.received_from_others
            );
        }
        let ids = self.requests.lock().unwrap().clone();
        let mut expected_escrow = 0u64;
        for id in ids {
            let r = self.snapshot(&id).await;
            if r["state"] == "locked" || r["state"] == "disputed" {
                expected_escrow += r["escrows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["notional"].as_u64().unwrap())
                    .sum::<u64>();
            }
        }
        assert_eq!(
            self.ledger.escrowed_total().minor_units(),
            expected_escrow,
            "venue escrow must equal the notionals of Locked/Disputed requests"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------------------------

/// Notional per leg in [`ThreeLeg`].
pub const LEG_NOTIONAL: u64 = 1_000;
/// Yes price every maker quotes at in [`ThreeLeg`].
pub const LEG_PRICE_BPS: u32 = 5_000;
/// Each side of each leg at that price.
pub const SIDE_LOCK: u64 = 500;
/// `t0 + RESPONSE_DEADLINE_SECS` is the response deadline; the accept window follows.
pub const RESPONSE_DEADLINE_SECS: i64 = 30;
pub const ACCEPT_DEADLINE_SECS: i64 = RESPONSE_DEADLINE_SECS + ACCEPT_WINDOW_SECS;
pub const QUOTE_EXPIRY_SECS: i64 = 600;

/// Requester R with three `buy_yes` legs; makers M1..M3 each funded to cover exactly one leg
/// (`SIDE_LOCK`). R is funded to cover its side of all three legs unless overridden.
pub struct ThreeLeg {
    pub requester: Uuid,
    pub makers: [Uuid; 3],
    pub request_id: String,
    pub leg_ids: [String; 3],
}

impl TestVenue {
    pub async fn three_leg_scenario(&self) -> ThreeLeg {
        self.three_leg_scenario_with_requester_funds(3 * SIDE_LOCK)
            .await
    }

    pub async fn three_leg_scenario_with_requester_funds(&self, requester_funds: u64) -> ThreeLeg {
        let requester = Uuid::new_v4();
        let makers = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        self.fund(requester, requester_funds).await;
        for m in makers {
            self.fund(m, SIDE_LOCK).await;
        }
        let legs = json!([
            leg("buy_yes", LEG_NOTIONAL),
            leg("buy_yes", LEG_NOTIONAL),
            leg("buy_yes", LEG_NOTIONAL)
        ]);
        let (status, created) = self
            .open_request(requester, legs, self.at(RESPONSE_DEADLINE_SECS))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let leg_ids = <[String; 3]>::try_from(leg_ids(&created)).unwrap();
        ThreeLeg {
            requester,
            makers,
            request_id: id_of(&created),
            leg_ids,
        }
    }

    /// Maker `i` quotes leg `i` at `LEG_PRICE_BPS` for the full notional; returns the quote id.
    pub async fn quote_leg(&self, s: &ThreeLeg, i: usize) -> String {
        self.quote_ok(
            s.makers[i],
            &s.request_id,
            &s.leg_ids[i],
            LEG_PRICE_BPS,
            LEG_NOTIONAL,
            self.at(QUOTE_EXPIRY_SECS),
        )
        .await
    }

    /// Quote all three legs; returns the quote ids in leg order.
    pub async fn quote_all_legs(&self, s: &ThreeLeg) -> [String; 3] {
        [
            self.quote_leg(s, 0).await,
            self.quote_leg(s, 1).await,
            self.quote_leg(s, 2).await,
        ]
    }
}

// ---------------------------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------------------------

pub fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

pub fn leg_ids(request: &Value) -> Vec<String> {
    request["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(id_of)
        .collect()
}

pub fn quote_state<'a>(request: &'a Value, quote_id: &str) -> &'a str {
    request["quotes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"] == quote_id)
        .unwrap_or_else(|| panic!("quote {quote_id} missing from {request}"))["state"]
        .as_str()
        .unwrap()
}

/// Assert each `(quote_id, state)` pair against the request snapshot.
pub fn assert_quote_states(request: &Value, expected: &[(&str, &str)]) {
    for (quote_id, state) in expected {
        assert_eq!(
            quote_state(request, quote_id),
            *state,
            "state of quote {quote_id}"
        );
    }
}
