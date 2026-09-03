//! Demo: the happy path and two failure paths, driven against the engine in-process with a
//! frozen clock so the output is deterministic. Balances are printed before and after each
//! money-moving step. Run with `cargo run --example demo`.
//!
//! The HTTP surface over the same engine is exercised by the integration tests in `tests/`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rfq_matching_settlement_engine::domain::{
    Amount, ContractDescription, ContractId, Leg, LegSide, OracleOutcome, PartyId, Price,
    RfqRequest, Tenor,
};
use rfq_matching_settlement_engine::engine::{Engine, EngineConfig, EngineHandle, spawn_engine};
use rfq_matching_settlement_engine::mock::{MockClock, MockLedger};

const ACCEPT_WINDOW: Duration = Duration::seconds(60);

struct Venue {
    engine: EngineHandle,
    clock: Arc<MockClock>,
    ledger: Arc<MockLedger>,
    t0: DateTime<Utc>,
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
                accept_window: ACCEPT_WINDOW,
                ..EngineConfig::default()
            },
        );
        let (engine, _actor) = spawn_engine(engine);
        Self {
            engine,
            clock,
            ledger,
            t0,
        }
    }

    fn at(&self, secs: i64) -> DateTime<Utc> {
        self.t0 + Duration::seconds(secs)
    }

    /// Move the frozen clock and deliver the worker's `Tick` by hand.
    async fn tick_at(&self, secs: i64) {
        self.clock.set(self.at(secs));
        self.engine.tick(self.at(secs)).await.unwrap();
    }

    async fn print_balances(&self, label: &str, parties: &[(&str, PartyId)]) {
        println!("  {label}");
        for (name, party) in parties {
            let a = self.engine.balance(*party).await.unwrap();
            println!(
                "    {name:<10} free {:>6}  reserved {:>6}  escrowed {:>6}",
                a.free, a.reserved, a.escrowed
            );
        }
        assert!(
            self.ledger.conservation_holds(),
            "ledger conservation violated"
        );
    }
}

fn leg(contract: &str, side: LegSide, notional: u64) -> Leg {
    Leg::new(
        ContractId::new(contract).unwrap(),
        ContractDescription::new(format!(
            "Settles Yes if {contract}/USD on Coinbase is above the strike 100000.00 at resolution; otherwise No."
        ))
        .unwrap(),
        side,
        Amount::new(notional),
    )
    .unwrap()
}

fn state(r: &RfqRequest) -> String {
    match r.fail_reason {
        Some(reason) => format!("{:?} ({reason:?})", r.state),
        None => format!("{:?}", r.state),
    }
}

/// Requester buys Yes on two legs; two makers compete on each; the better price wins, the
/// loser is released at accept, and Yes pays each leg's notional to the requester.
async fn happy_path() {
    println!("\n== 1. Happy path: two legs, accepted, resolved Yes ==");
    let v = Venue::new();
    let (r, m1, m2) = (PartyId::new(), PartyId::new(), PartyId::new());
    let parties = [("requester", r), ("maker1", m1), ("maker2", m2)];
    for (_, p) in parties {
        v.engine.credit(p, Amount::new(10_000)).await.unwrap();
    }
    v.print_balances("after faucet", &parties).await;

    let legs = vec![
        leg("A", LegSide::BuyYes, 1_000),
        leg("B", LegSide::BuyYes, 2_000),
    ];
    let req = v
        .engine
        .submit_request(r, legs, Tenor::FiveMinutes, v.at(30))
        .await
        .unwrap();
    let (leg_a, leg_b) = (req.legs[0].id, req.legs[1].id);
    println!("  request {} opened, state {}", req.id, state(&req));

    // Yes price in bps. On a BuyYes leg the maker is the Yes-seller and reserves (1 - p) * n.
    for (maker, leg_id, bps, size) in [
        (m1, leg_a, 4_000, 1_000),
        (m2, leg_a, 3_500, 1_000),
        (m1, leg_b, 6_000, 2_000),
        (m2, leg_b, 6_500, 2_000),
    ] {
        let q = v
            .engine
            .submit_quote(
                maker,
                req.id,
                leg_id,
                Price::new(bps).unwrap(),
                Amount::new(size),
                v.at(600),
            )
            .await
            .unwrap();
        println!(
            "  quote {} on leg {} at {bps} bps by {}",
            q.id,
            leg_id,
            if maker == m1 { "maker1" } else { "maker2" }
        );
    }
    v.print_balances(
        "after quotes: maker collateral reserved, requester untouched",
        &parties,
    )
    .await;

    v.tick_at(30).await;
    let req = v.engine.get_request(req.id).await.unwrap();
    println!("  tick at response deadline: state {}", state(&req));
    for s in &req.package.as_ref().unwrap().selections {
        println!("    leg {} -> quote {}", s.leg_id, s.quote_id);
    }

    let req = v.engine.accept(r, req.id).await.unwrap();
    println!("  requester accepts: state {}", state(&req));
    v.print_balances("after accept: one lock_batch, losers released", &parties)
        .await;

    let req = v.engine.resolve(req.id, OracleOutcome::Yes).await.unwrap();
    println!(
        "  oracle reports Yes: state {} (escrow held for the dispute window)",
        state(&req)
    );
    v.print_balances("after report: nothing moves yet", &parties)
        .await;

    v.tick_at(30 + 61).await;
    let req = v.engine.get_request(req.id).await.unwrap();
    println!("  dispute window closed unfiled: state {}", state(&req));
    v.print_balances("after settlement: Yes-buyer receives n per leg", &parties)
        .await;
}

/// Three legs, quotes on only two. A provisional match is a reservation, not a lock: at the
/// deadline the whole request fails and every reservation returns.
async fn unmatched_leg() {
    println!("\n== 2. Failure: leg 2 of 3 never quoted ==");
    let v = Venue::new();
    let (r, m) = (PartyId::new(), PartyId::new());
    let parties = [("requester", r), ("maker", m)];
    v.engine.credit(r, Amount::new(5_000)).await.unwrap();
    v.engine.credit(m, Amount::new(5_000)).await.unwrap();

    let legs = vec![
        leg("A", LegSide::BuyYes, 1_000),
        leg("B", LegSide::BuyYes, 1_000),
        leg("C", LegSide::BuyYes, 1_000),
    ];
    let req = v
        .engine
        .submit_request(r, legs, Tenor::FiveMinutes, v.at(30))
        .await
        .unwrap();
    for leg_id in [req.legs[0].id, req.legs[2].id] {
        v.engine
            .submit_quote(
                m,
                req.id,
                leg_id,
                Price::new(5_000).unwrap(),
                Amount::new(1_000),
                v.at(600),
            )
            .await
            .unwrap();
    }
    v.print_balances("after quotes on legs A and C", &parties)
        .await;

    v.tick_at(30).await;
    let req = v.engine.get_request(req.id).await.unwrap();
    println!("  tick at response deadline: state {}", state(&req));
    v.print_balances(
        "after failure: every reservation released, lock_batch never called",
        &parties,
    )
    .await;
    assert_eq!(v.ledger.lock_batch_calls(), 0);
}

/// The package is presented but the requester cannot fund its side. `lock_batch` refuses
/// before touching any account; the request fails and the makers are released.
async fn requester_cannot_fund() {
    println!("\n== 3. Failure: requester short at accept ==");
    let v = Venue::new();
    let (r, m) = (PartyId::new(), PartyId::new());
    let parties = [("requester", r), ("maker", m)];
    v.engine.credit(r, Amount::new(499)).await.unwrap(); // needs 500 at 50%
    v.engine.credit(m, Amount::new(5_000)).await.unwrap();

    let req = v
        .engine
        .submit_request(
            r,
            vec![leg("A", LegSide::BuyYes, 1_000)],
            Tenor::FiveMinutes,
            v.at(30),
        )
        .await
        .unwrap();
    v.engine
        .submit_quote(
            m,
            req.id,
            req.legs[0].id,
            Price::new(5_000).unwrap(),
            Amount::new(1_000),
            v.at(600),
        )
        .await
        .unwrap();
    v.tick_at(30).await;
    println!(
        "  presented: state {}",
        state(&v.engine.get_request(req.id).await.unwrap())
    );
    v.print_balances("before accept", &parties).await;

    let err = v.engine.accept(r, req.id).await.unwrap_err();
    println!("  accept refused: {err}");
    let req = v.engine.get_request(req.id).await.unwrap();
    println!("  state {}", state(&req));
    v.print_balances(
        "after refused accept: nothing moved, maker released",
        &parties,
    )
    .await;
}

#[tokio::main]
async fn main() {
    happy_path().await;
    unmatched_leg().await;
    requester_cannot_fund().await;
    println!();
}
