//! Demo: the happy path and two failure paths, driven against the engine in-process with a
//! frozen clock so the output is identical every run. Every party, leg, and quote is printed
//! by name, every state change as `from → to`, and every balance line carries the arithmetic
//! behind it. Run with `cargo run --example demo`.
//!
//! The HTTP surface over the same engine is exercised by the integration tests in `tests/`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rfq_matching_settlement_engine::clock::Clock;
use rfq_matching_settlement_engine::domain::{
    Amount, ContractDescription, ContractId, FailReason, Leg, LegId, LegSide, OracleOutcome,
    PartyId, Price, QuoteId, RequestId, RfqRequest, Tenor,
};
use rfq_matching_settlement_engine::engine::{
    Engine, EngineConfig, EngineError, EngineHandle, spawn_engine,
};
use rfq_matching_settlement_engine::mock::{MockClock, MockLedger};

const ACCEPT_WINDOW_SECS: i64 = 60;
const DISPUTE_WINDOW_SECS: i64 = 60;
const RESPONSE_DEADLINE_SECS: i64 = 30;

/// One venue plus a name for every id it hands out, so nothing prints as a UUID.
struct Venue {
    engine: EngineHandle,
    clock: Arc<MockClock>,
    ledger: Arc<MockLedger>,
    t0: DateTime<Utc>,
    parties: Vec<(String, PartyId)>,
    legs: HashMap<LegId, String>,
    quotes: HashMap<QuoteId, String>,
}

impl Venue {
    fn new() -> Self {
        let t0 = DateTime::from_timestamp(1_756_728_000, 0).unwrap();
        let clock = Arc::new(MockClock::new(t0));
        let ledger = Arc::new(MockLedger::new());
        let engine = Engine::new(
            ledger.clone(),
            clock.clone(),
            EngineConfig {
                accept_window: Duration::seconds(ACCEPT_WINDOW_SECS),
                dispute_window: Duration::seconds(DISPUTE_WINDOW_SECS),
                ..EngineConfig::default()
            },
        );
        let (engine, _actor) = spawn_engine(engine);
        Self {
            engine,
            clock,
            ledger,
            t0,
            parties: Vec::new(),
            legs: HashMap::new(),
            quotes: HashMap::new(),
        }
    }

    async fn party(&mut self, name: &str, funds: u64) -> PartyId {
        let id = PartyId::new();
        self.engine.credit(id, Amount::new(funds)).await.unwrap();
        self.parties.push((name.to_owned(), id));
        id
    }

    fn name(&self, id: PartyId) -> &str {
        self.parties
            .iter()
            .find(|(_, p)| *p == id)
            .map(|(n, _)| n.as_str())
            .unwrap_or("?")
    }

    fn at(&self, secs: i64) -> DateTime<Utc> {
        self.t0 + Duration::seconds(secs)
    }

    fn secs_now(&self) -> i64 {
        (self.clock.now() - self.t0).num_seconds()
    }

    async fn open(&mut self, requester: PartyId, legs: Vec<(&str, LegSide, u64)>) -> RfqRequest {
        let legs: Vec<Leg> = legs
            .into_iter()
            .map(|(name, side, notional)| {
                Leg::new(
                    ContractId::new(name).unwrap(),
                    ContractDescription::new(format!(
                        "Settles Yes if {name}/USD on Coinbase is above the strike 100000.00 at resolution; otherwise No."
                    ))
                    .unwrap(),
                    side,
                    Amount::new(notional),
                )
                .unwrap()
            })
            .collect();
        let req = self
            .engine
            .submit_request(
                requester,
                legs,
                Tenor::FiveMinutes,
                self.at(RESPONSE_DEADLINE_SECS),
            )
            .await
            .unwrap();
        for leg in &req.legs {
            self.legs.insert(leg.id, leg.contract.as_str().to_owned());
        }
        let legs: Vec<String> = req
            .legs
            .iter()
            .map(|l| format!("{} {:?} n={}", self.legs[&l.id], l.side, l.notional))
            .collect();
        println!(
            "  {} opens request [{}], deadline t+{RESPONSE_DEADLINE_SECS}s, tenor 5 min       state: Open",
            self.name(requester),
            legs.join(", ")
        );
        req
    }

    async fn quote(&mut self, maker: PartyId, req: &RfqRequest, leg: &str, bps: u32) -> QuoteId {
        let leg = req
            .legs
            .iter()
            .find(|l| l.contract.as_str() == leg)
            .expect("leg by name");
        let q = self
            .engine
            .submit_quote(
                maker,
                req.id,
                leg.id,
                Price::new(bps).unwrap(),
                leg.notional,
                self.at(600),
            )
            .await
            .unwrap();
        let label = format!("{}@{bps}", self.name(maker));
        self.quotes.insert(q.id, label.clone());
        let n = leg.notional.minor_units();
        let (role, lock) = if leg.side.requester_buys_yes() {
            ("Yes-seller", n - n * u64::from(bps) / 10_000)
        } else {
            ("Yes-buyer", n * u64::from(bps) / 10_000)
        };
        println!(
            "  {label:<12} quotes {} at {bps} bps; maker is the {role}, reserves {lock}",
            self.legs[&leg.id]
        );
        q.id
    }

    /// Move the frozen clock and deliver the worker's `Tick` by hand.
    async fn tick_at(&self, secs: i64) {
        self.clock.set(self.at(secs));
        self.engine.tick(self.at(secs)).await.unwrap();
    }

    async fn snapshot(&self, id: RequestId) -> RfqRequest {
        self.engine.get_request(id).await.unwrap()
    }

    fn describe(&self, r: &RfqRequest) -> String {
        match r.fail_reason {
            Some(FailReason::LegUnmatched(leg)) => {
                format!("Failed, leg {} unmatched", self.legs[&leg])
            }
            Some(reason) => format!("Failed, {reason:?}"),
            None => format!("{:?}", r.state),
        }
    }

    async fn balances(&self, label: &str) {
        println!("  {label}");
        for (name, id) in &self.parties {
            let a = self.engine.balance(*id).await.unwrap();
            println!(
                "      {name:<10} free {:>6}   reserved {:>5}   escrowed {:>5}",
                a.free, a.reserved, a.escrowed
            );
        }
        assert!(
            self.ledger.conservation_holds(),
            "ledger conservation violated"
        );
    }
}

fn transition(from: &str, to: &str, why: &str) {
    println!("  ── {why}: {from} → {to}");
}

/// Requester buys Yes on A and sells Yes on B. Two makers compete on each leg; the better
/// Yes price wins each leg independently, the loser's collateral comes back at accept, and a
/// reported Yes pays each leg's Yes-buyer the notional once the dispute window closes unfiled.
async fn happy_path() {
    println!("\n══ 1. Happy path: two legs, two makers, accepted, reported Yes, window closes ══");
    let mut v = Venue::new();
    let r = v.party("requester", 10_000).await;
    let m1 = v.party("maker1", 10_000).await;
    let m2 = v.party("maker2", 10_000).await;
    v.balances("t+0s  faucet").await;

    let req = v
        .open(
            r,
            vec![
                ("A", LegSide::BuyYes, 1_000),
                ("B", LegSide::SellYes, 2_000),
            ],
        )
        .await;
    println!(
        "        A: requester buys Yes, wants the LOWEST Yes price;  B: requester sells Yes, wants the HIGHEST"
    );
    v.quote(m1, &req, "A", 4_000).await;
    v.quote(m2, &req, "A", 3_500).await;
    v.quote(m1, &req, "B", 6_000).await;
    v.quote(m2, &req, "B", 6_500).await;
    v.balances(
        "t+0s  after quotes: maker1 reserved 600+1200, maker2 650+1300; requester untouched",
    )
    .await;

    v.tick_at(RESPONSE_DEADLINE_SECS).await;
    let req = v.snapshot(req.id).await;
    transition(
        "Open",
        &v.describe(&req),
        "tick at the response deadline, best quote per leg",
    );
    for s in &req.package.as_ref().unwrap().selections {
        println!("        {} ← {}", v.legs[&s.leg_id], v.quotes[&s.quote_id]);
    }
    println!(
        "        (A: lowest Yes price wins; B: highest wins. maker1 lost both legs and stays reserved until accept)"
    );

    let req = v.engine.accept(r, req.id).await.unwrap();
    transition(
        "Presented",
        &v.describe(&req),
        "requester accepts, one lock_batch",
    );
    for e in &req.escrows {
        println!(
            "        {}: {} escrows {} as Yes-buyer, {} escrows {} as Yes-seller  (sum {})",
            v.legs[&e.leg_id],
            v.name(e.yes_buyer),
            e.yes_buyer_amount,
            v.name(e.yes_seller),
            e.yes_seller_amount,
            e.notional
        );
    }
    v.balances("t+30s after accept: winners' reservations became escrow, losers released to free")
        .await;

    let req = v.engine.resolve(req.id, OracleOutcome::Yes).await.unwrap();
    transition("Locked", &v.describe(&req), "oracle reports Yes");
    v.balances(&format!(
        "t+{}s after report: nothing moves, {DISPUTE_WINDOW_SECS}s dispute window open",
        v.secs_now()
    ))
    .await;

    v.tick_at(RESPONSE_DEADLINE_SECS + DISPUTE_WINDOW_SECS + 1)
        .await;
    let req = v.snapshot(req.id).await;
    transition(
        "Reported",
        &v.describe(&req),
        "tick past the dispute window, nobody filed",
    );
    v.balances(&format!(
        "t+{}s after settlement: each leg's Yes-buyer receives n (A: requester +1000, B: maker2 +2000)",
        v.secs_now()
    ))
    .await;
}

/// Three legs, quotes on only two. A provisional match is a reservation, not a lock: at the
/// deadline the whole request fails and every reservation returns.
async fn unmatched_leg() {
    println!("\n══ 2. Failure: leg B of three never quoted ══");
    let mut v = Venue::new();
    let r = v.party("requester", 5_000).await;
    let m = v.party("maker", 5_000).await;
    let req = v
        .open(
            r,
            vec![
                ("A", LegSide::BuyYes, 1_000),
                ("B", LegSide::BuyYes, 1_000),
                ("C", LegSide::BuyYes, 1_000),
            ],
        )
        .await;
    v.quote(m, &req, "A", 5_000).await;
    v.quote(m, &req, "C", 5_000).await;
    v.balances("t+0s  after quotes on A and C only").await;

    v.tick_at(RESPONSE_DEADLINE_SECS).await;
    let req = v.snapshot(req.id).await;
    transition("Open", &v.describe(&req), "tick at the response deadline");
    v.balances("t+30s after failure: both reservations released, lock_batch never called")
        .await;
    assert_eq!(v.ledger.lock_batch_calls(), 0);
}

/// The package is presented but the requester cannot fund its side. `lock_batch` refuses
/// before touching any account; the request fails and the maker is released.
async fn requester_cannot_fund() {
    println!("\n══ 3. Failure: requester is 1 short at accept ══");
    let mut v = Venue::new();
    let r = v.party("requester", 499).await;
    let m = v.party("maker", 5_000).await;
    let req = v.open(r, vec![("A", LegSide::BuyYes, 1_000)]).await;
    v.quote(m, &req, "A", 5_000).await;
    v.tick_at(RESPONSE_DEADLINE_SECS).await;
    let req = v.snapshot(req.id).await;
    transition("Open", &v.describe(&req), "tick at the response deadline");
    v.balances("t+30s before accept: requester needs 500 (p·n = 50% of 1000), has 499")
        .await;

    let err = v.engine.accept(r, req.id).await.unwrap_err();
    match err {
        EngineError::InsufficientFunds {
            party,
            needed,
            available,
        } => println!(
            "  accept refused: {} needs {needed}, has {available}",
            v.name(party)
        ),
        other => println!("  accept refused: {other}"),
    }
    let req = v.snapshot(req.id).await;
    transition(
        "Presented",
        &v.describe(&req),
        "lock_batch refused before mutating anything",
    );
    v.balances("t+30s after refused accept: requester unchanged, maker released")
        .await;
}

#[tokio::main]
async fn main() {
    happy_path().await;
    unmatched_leg().await;
    requester_cannot_fund().await;
    println!();
}
