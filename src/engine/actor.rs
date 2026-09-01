use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::core::Engine;
use crate::domain::{
    Amount, Command, EngineError, LedgerAccount, Leg, LegId, OracleOutcome, PartyId, Price,
    Quote, QuoteId, Reply, RequestId, RfqRequest,
};

/// Cloneable client for the engine actor. Every method sends one [`Command`] and awaits its
/// one-shot reply.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
}

/// Run `engine` on its own task. Returns the client handle and the task's `JoinHandle`; the
/// actor exits when the last handle is dropped.
pub fn spawn_engine(mut engine: Engine) -> (EngineHandle, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Command>(256);
    let task = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            engine.handle(cmd);
        }
    });
    (EngineHandle { tx }, task)
}

impl EngineHandle {
    async fn ask<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(build(reply)).await.map_err(|_| EngineError::Unavailable)?;
        rx.await.map_err(|_| EngineError::Unavailable)?
    }

    pub async fn submit_request(
        &self,
        requester: PartyId,
        legs: Vec<Leg>,
        response_deadline: DateTime<Utc>,
    ) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::SubmitRequest { requester, legs, response_deadline, reply }).await
    }

    pub async fn submit_quote(
        &self,
        maker: PartyId,
        request_id: RequestId,
        leg_id: LegId,
        price: Price,
        size: Amount,
        expires_at: DateTime<Utc>,
    ) -> Result<Quote, EngineError> {
        self.ask(|reply| Command::SubmitQuote { maker, request_id, leg_id, price, size, expires_at, reply })
            .await
    }

    pub async fn cancel_quote(&self, maker: PartyId, quote_id: QuoteId) -> Result<(), EngineError> {
        self.ask(|reply| Command::CancelQuote { maker, quote_id, reply }).await
    }

    pub async fn accept(&self, requester: PartyId, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Accept { requester, request_id, reply }).await
    }

    pub async fn reject(&self, requester: PartyId, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Reject { requester, request_id, reply }).await
    }

    pub async fn resolve(&self, request_id: RequestId, outcome: OracleOutcome) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::Resolve { request_id, outcome, reply }).await
    }

    pub async fn get_request(&self, request_id: RequestId) -> Result<RfqRequest, EngineError> {
        self.ask(|reply| Command::GetRequest { request_id, reply }).await
    }

    pub async fn credit(&self, party: PartyId, amount: Amount) -> Result<LedgerAccount, EngineError> {
        self.ask(|reply| Command::Credit { party, amount, reply }).await
    }

    pub async fn balance(&self, party: PartyId) -> Result<LedgerAccount, EngineError> {
        self.ask(|reply| Command::Balance { party, reply }).await
    }

    /// Fire-and-forget heartbeat. Commands are processed in order, so anything sent after
    /// this observes its effects.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<(), EngineError> {
        self.tx.send(Command::Tick { now }).await.map_err(|_| EngineError::Unavailable)
    }
}
