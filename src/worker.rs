//! Expiry worker: sends `Tick { now }` to the engine on a fixed period.

use std::time::Duration;

use tokio::task::JoinHandle;

use crate::engine::{EngineHandle, SharedClock};

/// Tick the engine every `period` until the engine goes away.
pub fn spawn_expiry_worker(engine: EngineHandle, clock: SharedClock, period: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            if engine.tick(clock.now()).await.is_err() {
                break;
            }
        }
    })
}
