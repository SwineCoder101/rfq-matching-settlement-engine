use std::sync::Arc;
use std::time::Duration;

use rfq_matching_settlement_engine::api::{AppState, router};
use rfq_matching_settlement_engine::clock::SystemClock;
use rfq_matching_settlement_engine::engine::{Engine, EngineConfig, SharedClock, spawn_engine};
use rfq_matching_settlement_engine::mock::MockLedger;
use rfq_matching_settlement_engine::worker::spawn_expiry_worker;

#[tokio::main]
async fn main() {
    let addr = std::env::var("RFQ_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());

    let clock: SharedClock = Arc::new(SystemClock);
    let engine = Engine::new(
        Arc::new(MockLedger::new()),
        clock.clone(),
        EngineConfig::default(),
    );
    let (handle, actor) = spawn_engine(engine);
    let worker = spawn_expiry_worker(handle.clone(), clock, Duration::from_millis(500));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    println!("rfq engine listening on http://{addr}");
    if let Err(e) = axum::serve(listener, router(AppState { engine: handle })).await {
        eprintln!("server error: {e}");
    }
    worker.abort();
    actor.abort();
}
