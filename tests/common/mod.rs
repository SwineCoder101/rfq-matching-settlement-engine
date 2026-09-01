//! Shared harness for HTTP-level tests: an in-process venue with a mock clock and ledger,
//! typed endpoint helpers, JSON fixtures with `{{placeholder}}` substitution, and assertions.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use rfq_matching_settlement_engine::api::{AppState, PARTY_HEADER, router};
use rfq_matching_settlement_engine::engine::{Engine, EngineConfig, EngineHandle, spawn_engine};
use rfq_matching_settlement_engine::mocks::{MockClock, MockLedger};

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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let mut text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    for (key, value) in vars {
        text = text.replace(&format!("{{{{{key}}}}}"), &value);
    }
    assert!(!text.contains("{{"), "fixture {name} has unsubstituted placeholders:\n{text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}\n{text}"))
}

/// A timestamp exactly as the server serializes it, for use in fixtures.
pub fn ts(t: DateTime<Utc>) -> String {
    serde_json::to_value(t).unwrap().as_str().unwrap().to_owned()
}

// ---------------------------------------------------------------------------------------------
// Venue
// ---------------------------------------------------------------------------------------------

/// Free / reserved / escrowed, in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balances {
    pub free: u64,
    pub reserved: u64,
    pub escrowed: u64,
}

pub const fn bal(free: u64, reserved: u64, escrowed: u64) -> Balances {
    Balances { free, reserved, escrowed }
}

pub struct Venue {
    app: Router,
    engine: EngineHandle,
    clock: Arc<MockClock>,
    pub ledger: Arc<MockLedger>,
    t0: DateTime<Utc>,
}

impl Venue {
    pub fn start() -> Self {
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

    /// `t0 + secs`.
    pub fn at(&self, secs: i64) -> DateTime<Utc> {
        self.t0 + Duration::seconds(secs)
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now_value()
    }

    /// Move the mock clock and tick the engine, as the expiry worker would.
    pub async fn advance_to(&self, secs: i64) {
        let now = self.at(secs);
        self.clock.set(now);
        self.engine.tick(now).await.unwrap();
    }

    // ---- raw HTTP --------------------------------------------------------------------------

    pub async fn call(&self, method: Method, path: &str, party: Option<Uuid>, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(p) = party {
            builder = builder.header(PARTY_HEADER, p.to_string());
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
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }))
        };
        (status, json)
    }

    async fn expect(&self, expected: StatusCode, method: Method, path: &str, party: Option<Uuid>, body: Option<Value>) -> Value {
        let (status, json) = self.call(method.clone(), path, party, body).await;
        assert_eq!(status, expected, "{method} {path} → {json}");
        json
    }

    // ---- ledger ----------------------------------------------------------------------------

    pub async fn credit(&self, party: Uuid, amount: u64) {
        let body = fixture("requests/credit.json", vars!["party_id" => party, "amount" => amount]);
        self.expect(StatusCode::OK, Method::POST, "/v1/ledger/credit", None, Some(body)).await;
    }

    pub async fn balance(&self, party: Uuid) -> Balances {
        let json = self.expect(StatusCode::OK, Method::GET, &format!("/v1/ledger/{party}"), None, None).await;
        Balances {
            free: json["free"].as_u64().unwrap(),
            reserved: json["reserved"].as_u64().unwrap(),
            escrowed: json["escrowed"].as_u64().unwrap(),
        }
    }

    /// Assert several parties' balances at once, and that the ledger still conserves funds.
    pub async fn assert_balances(&self, expected: &[(&str, Uuid, Balances)]) {
        for (label, party, want) in expected {
            let got = self.balance(*party).await;
            assert_eq!(got, *want, "balances of {label}");
        }
        assert!(self.ledger.conservation_holds(), "ledger conservation violated");
    }

    // ---- requests --------------------------------------------------------------------------

    /// `POST /v1/requests` with a request fixture; returns the 201 body.
    pub async fn create_request(&self, requester: Uuid, fixture_name: &str, response_deadline: DateTime<Utc>) -> Value {
        let body = fixture(fixture_name, vars!["response_deadline" => ts(response_deadline)]);
        self.expect(StatusCode::CREATED, Method::POST, "/v1/requests", Some(requester), Some(body)).await
    }

    pub async fn get_request(&self, id: &str) -> Value {
        self.expect(StatusCode::OK, Method::GET, &format!("/v1/requests/{id}"), None, None).await
    }

    pub async fn accept(&self, requester: Uuid, id: &str) -> (StatusCode, Value) {
        self.call(Method::POST, &format!("/v1/requests/{id}/accept"), Some(requester), None).await
    }

    // ---- quotes ----------------------------------------------------------------------------

    /// `POST /v1/requests/{id}/quotes`; returns the new quote id.
    pub async fn quote(&self, maker: Uuid, request_id: &str, leg_id: &str, price_bps: u32, size: u64, expires_at: DateTime<Utc>) -> String {
        let body = fixture(
            "requests/quote.json",
            vars!["leg_id" => leg_id, "price_bps" => price_bps, "size" => size, "expires_at" => ts(expires_at)],
        );
        let json = self
            .expect(StatusCode::CREATED, Method::POST, &format!("/v1/requests/{request_id}/quotes"), Some(maker), Some(body))
            .await;
        assert_eq!(json["state"], "live");
        json["id"].as_str().unwrap().to_owned()
    }

    pub async fn cancel_quote(&self, maker: Uuid, quote_id: &str) {
        self.expect(StatusCode::NO_CONTENT, Method::DELETE, &format!("/v1/quotes/{quote_id}"), Some(maker), None).await;
    }

    // ---- oracle ----------------------------------------------------------------------------

    pub async fn resolve(&self, request_id: &str, outcome: &str) -> (StatusCode, Value) {
        let body = fixture("requests/resolve.json", vars!["request_id" => request_id, "outcome" => outcome]);
        self.call(Method::POST, "/v1/oracle/resolve", None, Some(body)).await
    }
}

// ---------------------------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------------------------

pub fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap_or_else(|| panic!("no id in {v}")).to_owned()
}

pub fn leg_ids(request: &Value) -> Vec<String> {
    request["legs"].as_array().unwrap().iter().map(id_of).collect()
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
        assert_eq!(quote_state(request, quote_id), *state, "state of quote {quote_id}");
    }
}
