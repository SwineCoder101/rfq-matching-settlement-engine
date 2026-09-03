//! Failure modes and races. Every test here is a row in `FAILURE_MODES.md`, and every row
//! there names a test here. Each test asserts HTTP status, request/quote states, balances,
//! and ends with `assert_conserved()`.
//!
//! Time is driven explicitly: the expiry worker is not running, `advance_to` / `tick_at`
//! deliver `Tick` by hand, so every interleaving below is reproducible.

mod common;

use axum::http::{Method, StatusCode};
use common::{
    ACCEPT_DEADLINE_SECS, LEG_NOTIONAL, LEG_PRICE_BPS, QUOTE_EXPIRY_SECS, RESPONSE_DEADLINE_SECS,
    SIDE_LOCK, TestVenue, assert_quote_states, bal, id_of, leg, leg_ids, ts,
};
use rfq_matching_settlement_engine::domain::{
    Amount, InsufficientFunds, Ledger, LockBatchError, LockItem, PartyId,
};
use serde_json::{Value, json};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------
// Local scenario: one buy_yes leg, two makers at different prices
// ---------------------------------------------------------------------------------------------

/// R funded 600, M1 and M2 funded 500. M1 quotes at 50% (locks 500), M2 at 60% (locks 400).
/// R's lock is 500 if M1 wins, 600 if M2 wins.
struct OneLeg {
    requester: Uuid,
    m1: Uuid,
    m2: Uuid,
    request_id: String,
    leg_id: String,
}

const M1_PRICE: u32 = 5_000;
const M2_PRICE: u32 = 6_000;
const M2_LOCK: u64 = 400;

async fn one_leg(v: &TestVenue) -> OneLeg {
    let (requester, m1, m2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    v.fund(requester, 600).await;
    v.fund(m1, SIDE_LOCK).await;
    v.fund(m2, SIDE_LOCK).await;
    let (status, created) = v.open_request(requester, json!([leg("buy_yes", LEG_NOTIONAL)]), v.at(RESPONSE_DEADLINE_SECS)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    OneLeg { requester, m1, m2, request_id: id_of(&created), leg_id: leg_ids(&created).remove(0) }
}

impl OneLeg {
    async fn quote_m1(&self, v: &TestVenue, expires_secs: i64) -> String {
        v.quote_ok(self.m1, &self.request_id, &self.leg_id, M1_PRICE, LEG_NOTIONAL, v.at(expires_secs)).await
    }
    async fn quote_m2(&self, v: &TestVenue, expires_secs: i64) -> String {
        v.quote_ok(self.m2, &self.request_id, &self.leg_id, M2_PRICE, LEG_NOTIONAL, v.at(expires_secs)).await
    }
}

fn selections(r: &Value) -> Vec<String> {
    r["package"]["selections"].as_array().unwrap().iter().map(|s| s["quote_id"].as_str().unwrap().to_owned()).collect()
}

// =============================================================================================
// Happy path
// =============================================================================================

#[tokio::test]
async fn fm_happy_path_three_legs() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, SIDE_LOCK, 0));
    }
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0), "requester stays free while Open");

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let presented = v.snapshot(&s.request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(selections(&presented), quotes);

    let (status, locked) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["state"], "locked");
    let escrows = locked["escrows"].as_array().unwrap();
    assert_eq!(escrows.len(), 3);
    for (e, (leg_id, maker)) in escrows.iter().zip(s.leg_ids.iter().zip(s.makers)) {
        assert_eq!(e["leg_id"], *leg_id);
        assert_eq!(e["yes_buyer"], s.requester.to_string());
        assert_eq!(e["yes_seller"], maker.to_string());
        assert_eq!((e["yes_buyer_amount"].as_u64(), e["yes_seller_amount"].as_u64()), (Some(SIDE_LOCK), Some(SIDE_LOCK)));
    }
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, SIDE_LOCK));
    }
    v.assert_conserved().await;

    let (status, settled) = v.resolve(v.oracle_party(), &s.request_id, "yes").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    assert_eq!(v.balances(s.requester).await, bal(3 * LEG_NOTIONAL, 0, 0), "winner receives n per leg");
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, 0));
    }
    for q in &quotes {
        assert_quote_states(&settled, &[(q, "locked")]);
    }
    v.assert_conserved().await;
}

// =============================================================================================
// Multi-leg / partial failure
// =============================================================================================

#[tokio::test]
async fn fm_leg_unmatched_at_deadline_releases_all() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let q0 = v.quote_leg(&s, 0).await;
    let q2 = v.quote_leg(&s, 2).await;
    assert_eq!(v.balances(s.makers[0]).await, bal(0, SIDE_LOCK, 0));
    assert_eq!(v.balances(s.makers[2]).await, bal(0, SIDE_LOCK, 0));

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["fail_reason"], json!({ "reason": "leg_unmatched", "leg_id": s.leg_ids[1] }));
    assert!(failed.get("package").is_none(), "requester never sees a partial package");
    assert_eq!(failed["escrows"], json!([]));
    assert_quote_states(&failed, &[(&q0, "released"), (&q2, "released")]);

    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    assert_eq!(v.ledger.lock_batch_calls(), 0, "lock_batch is never called on a failed match");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_requester_insufficient_funds_at_accept() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario_with_requester_funds(3 * SIDE_LOCK - 1).await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;

    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["code"], "insufficient_funds");

    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["fail_reason"], json!({ "reason": "insufficient_requester_funds" }));
    assert_eq!(failed["escrows"], json!([]));
    for q in &quotes {
        assert_quote_states(&failed, &[(q, "released")]);
    }
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK - 1, 0, 0), "failed batch mutated nothing");
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.ledger.lock_batch_calls(), 1);
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_lock_batch_is_atomic() {
    let v = TestVenue::new();
    let ledger = &*v.ledger;
    let (a, b, c) = (PartyId::new(), PartyId::new(), PartyId::new());
    ledger.credit(a, Amount::new(100));
    ledger.credit(b, Amount::new(100));
    ledger.credit(c, Amount::new(10));
    let reservation = ledger.reserve(a, Amount::new(40)).unwrap();
    let before = [ledger.balance(a), ledger.balance(b), ledger.balance(c)];

    let err = ledger
        .lock_batch(vec![
            LockItem::FromReservation(reservation),
            LockItem::FromFree { party: b, amount: Amount::new(60) },
            LockItem::FromFree { party: c, amount: Amount::new(50) }, // short by 40
        ])
        .unwrap_err();
    assert_eq!(
        err,
        LockBatchError::InsufficientFunds(InsufficientFunds { party: c, needed: Amount::new(50), available: Amount::new(10) })
    );
    assert_eq!([ledger.balance(a), ledger.balance(b), ledger.balance(c)], before, "first two items rolled back");
    assert_eq!(ledger.escrowed_total(), Amount::ZERO);
    ledger.release(reservation);
    v.assert_conserved().await;
}

// =============================================================================================
// Accept vs expiry
// =============================================================================================

#[tokio::test]
async fn fm_requester_reject_releases_selected_and_losers() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let q1 = s.quote_m1(&v, QUOTE_EXPIRY_SECS).await;
    let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let presented = v.snapshot(&s.request_id).await;
    assert_quote_states(&presented, &[(&q1, "selected"), (&q2, "live")]);

    let (status, failed) = v.reject(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::OK, "{failed}");
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["fail_reason"], json!({ "reason": "rejected" }));
    assert_quote_states(&failed, &[(&q1, "released"), (&q2, "released")]);
    v.assert_balances(&[
        ("requester", s.requester, bal(600, 0, 0)),
        ("m1", s.m1, bal(SIDE_LOCK, 0, 0)),
        ("m2", s.m2, bal(SIDE_LOCK, 0, 0)),
    ])
    .await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_accept_window_expiry_fails_request() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented");

    v.advance_to(ACCEPT_DEADLINE_SECS).await;
    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented", "accept is allowed at the deadline instant");

    v.advance_to(ACCEPT_DEADLINE_SECS + 1).await;
    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["fail_reason"], json!({ "reason": "accept_window_expired" }));
    for q in &quotes {
        assert_quote_states(&failed, &[(q, "released")]);
    }
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_selected_quote_cannot_be_cancelled() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;

    let (status, body) = v.cancel_quote(s.makers[0], &quotes[0]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    assert_quote_states(&v.snapshot(&s.request_id).await, &[(&quotes[0], "selected")]);
    assert_eq!(v.balances(s.makers[0]).await, bal(0, SIDE_LOCK, 0), "reservation intact");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_quote_expiring_before_accept_window_rejected_at_submit() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let m = s.makers[0];

    let (status, body) = v.quote(m, &s.request_id, &s.leg_ids[0], LEG_PRICE_BPS, LEG_NOTIONAL, v.at(ACCEPT_DEADLINE_SECS - 1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_expires_before_accept_window");

    let (status, body) = v.quote(m, &s.request_id, &s.leg_ids[0], LEG_PRICE_BPS, LEG_NOTIONAL, v.now()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_expired");

    assert_eq!(v.snapshot(&s.request_id).await["quotes"], json!([]));
    assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0), "nothing reserved");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_quote_expiring_before_accept_deadline_is_ineligible() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    // M1 is the better price but expires at the earliest instant the submit guard allows.
    let q1 = s.quote_m1(&v, ACCEPT_DEADLINE_SECS).await;
    let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;

    // The worker ticks one second late, so the accept window now ends after M1 expires.
    v.advance_to(RESPONSE_DEADLINE_SECS + 1).await;
    let presented = v.snapshot(&s.request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(presented["accept_deadline"], ts(v.at(ACCEPT_DEADLINE_SECS + 1)));
    assert_eq!(selections(&presented), vec![q2.clone()], "worse but longer-lived quote wins");
    assert_quote_states(&presented, &[(&q1, "live"), (&q2, "selected")]);

    let (status, locked) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_quote_states(&locked, &[(&q1, "released"), (&q2, "locked")]);
    v.assert_balances(&[
        ("requester", s.requester, bal(0, 0, 600)),
        ("m1", s.m1, bal(SIDE_LOCK, 0, 0)),
        ("m2", s.m2, bal(SIDE_LOCK - M2_LOCK, 0, M2_LOCK)),
    ])
    .await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_expired_quote_not_selected_and_released() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let q1 = s.quote_m1(&v, ACCEPT_DEADLINE_SECS).await;
    let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;

    // Worker is very late: M1's quote has expired outright by the time the deadline is processed.
    v.advance_to(ACCEPT_DEADLINE_SECS + 5).await;
    let presented = v.snapshot(&s.request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(selections(&presented), vec![q2.clone()]);
    assert_quote_states(&presented, &[(&q1, "released"), (&q2, "selected")]);
    v.assert_balances(&[("m1", s.m1, bal(SIDE_LOCK, 0, 0)), ("m2", s.m2, bal(SIDE_LOCK - M2_LOCK, M2_LOCK, 0))]).await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_double_accept_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);

    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    assert_eq!(v.snapshot(&s.request_id).await["state"], "locked");
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK), "second accept locked nothing extra");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_accept_after_terminal_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    assert_eq!(v.resolve(v.oracle_party(), &s.request_id, "yes").await.0, StatusCode::OK);
    let after_settle = v.balances(s.requester).await;

    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::CONFLICT);
    assert_eq!(v.reject(s.requester, &s.request_id).await.0, StatusCode::CONFLICT);
    assert_eq!(v.resolve(v.oracle_party(), &s.request_id, "no").await.0, StatusCode::CONFLICT, "no second payout");
    assert_eq!(v.snapshot(&s.request_id).await["state"], "settled");
    assert_eq!(v.balances(s.requester).await, after_settle);
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_accept_and_tick_race() {
    let (mut locked_wins, mut expiry_wins) = (0, 0);
    for i in 0..50 {
        let v = TestVenue::new();
        let s = v.three_leg_scenario().await;
        v.quote_all_legs(&s).await;
        v.advance_to(RESPONSE_DEADLINE_SECS).await;

        // The clock is still inside the window, but the worker's Tick carries a `now` past it.
        let late = v.at(ACCEPT_DEADLINE_SECS + 1);
        let (status, _) = if i % 2 == 0 {
            let (accept, ()) = tokio::join!(v.accept(s.requester, &s.request_id), v.tick_at(late));
            accept
        } else {
            let ((), accept) = tokio::join!(v.tick_at(late), v.accept(s.requester, &s.request_id));
            accept
        };

        let r = v.snapshot(&s.request_id).await;
        match r["state"].as_str().unwrap() {
            "locked" => {
                locked_wins += 1;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(r["escrows"].as_array().unwrap().len(), 3);
                assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK));
            }
            "failed" => {
                expiry_wins += 1;
                assert_eq!(status, StatusCode::CONFLICT);
                assert_eq!(r["fail_reason"], json!({ "reason": "accept_window_expired" }));
                assert_eq!(r["escrows"], json!([]));
                assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
                for m in s.makers {
                    assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
                }
            }
            other => panic!("iteration {i}: impossible state {other}"),
        }
        v.assert_conserved().await;
    }
    assert!(locked_wins > 0 && expiry_wins > 0, "both interleavings exercised: {locked_wins} locked, {expiry_wins} expired");
}

#[tokio::test]
async fn fm_cancel_and_tick_race() {
    let (mut cancelled, mut selected) = (0, 0);
    for i in 0..50 {
        let v = TestVenue::new();
        let s = one_leg(&v).await;
        let q1 = s.quote_m1(&v, QUOTE_EXPIRY_SECS).await;
        let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;
        v.set(v.at(RESPONSE_DEADLINE_SECS));

        let (status, _) = if i % 2 == 0 {
            let (cancel, ()) = tokio::join!(v.cancel_quote(s.m1, &q1), v.tick());
            cancel
        } else {
            let ((), cancel) = tokio::join!(v.tick(), v.cancel_quote(s.m1, &q1));
            cancel
        };

        let r = v.snapshot(&s.request_id).await;
        assert_eq!(r["state"], "presented", "M2 keeps the leg matched either way");
        match status {
            StatusCode::NO_CONTENT => {
                cancelled += 1;
                assert_quote_states(&r, &[(&q1, "released"), (&q2, "selected")]);
                assert_eq!(v.balances(s.m1).await, bal(SIDE_LOCK, 0, 0));
            }
            StatusCode::CONFLICT => {
                selected += 1;
                assert_quote_states(&r, &[(&q1, "selected"), (&q2, "live")]);
                assert_eq!(v.balances(s.m1).await, bal(0, SIDE_LOCK, 0));
            }
            other => panic!("iteration {i}: unexpected {other}"),
        }
        v.assert_conserved().await;
    }
    assert!(cancelled > 0 && selected > 0, "both interleavings exercised: {cancelled} cancelled, {selected} selected");
}

// =============================================================================================
// Adversarial parties
// =============================================================================================

#[tokio::test]
async fn fm_non_owner_cannot_accept_or_reject() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let stranger = Uuid::new_v4();

    let (status, body) = v.accept(stranger, &s.request_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "not_owner");
    let (status, body) = v.reject(stranger, &s.request_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented");
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, SIDE_LOCK, 0));
    }
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
async fn fm_self_quote_rejected() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;

    let (status, body) = v.quote(s.requester, &s.request_id, &s.leg_id, M1_PRICE, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "requester must not quote its own request: {body}");
    assert_eq!(v.snapshot(&s.request_id).await["quotes"], json!([]));
    assert_eq!(v.balances(s.requester).await, bal(600, 0, 0), "nothing reserved");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_untrusted_party_cannot_resolve() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);

    let (status, body) = v.resolve(Uuid::new_v4(), &s.request_id, "yes").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only the oracle party may resolve: {body}");
    assert_eq!(v.snapshot(&s.request_id).await["state"], "locked");
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK), "funds untouched");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_maker_insufficient_funds_at_quote_is_402() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let poor = Uuid::new_v4();
    v.fund(poor, SIDE_LOCK - 1).await;

    let (status, body) = v.quote(poor, &s.request_id, &s.leg_id, M1_PRICE, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await;
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

    let (status, body) = v.quote(s.makers[0], &s.request_id, &s.leg_ids[0], LEG_PRICE_BPS, LEG_NOTIONAL - 1, v.at(QUOTE_EXPIRY_SECS)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "quote_too_small");
    assert_eq!(v.balances(s.makers[0]).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_quote_after_presented_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let late_maker = Uuid::new_v4();
    v.fund(late_maker, SIDE_LOCK).await;

    let (status, body) = v.quote(late_maker, &s.request_id, &s.leg_ids[0], LEG_PRICE_BPS - 1, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    assert_eq!(v.snapshot(&s.request_id).await["quotes"].as_array().unwrap().len(), 3);
    assert_eq!(v.balances(late_maker).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_cancel_released_quote_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let q0 = v.quote_leg(&s, 0).await;
    assert_eq!(v.cancel_quote(s.makers[0], &q0).await.0, StatusCode::NO_CONTENT);
    assert_eq!(v.balances(s.makers[0]).await, bal(SIDE_LOCK, 0, 0));

    let (status, body) = v.cancel_quote(s.makers[0], &q0).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "quote_not_live");
    assert_eq!(v.balances(s.makers[0]).await, bal(SIDE_LOCK, 0, 0), "no double release");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_tie_breaks_on_seq() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    // Same price, same frozen clock instant; M2 submits second.
    let q1 = s.quote_m1(&v, QUOTE_EXPIRY_SECS).await;
    let q2 = v.quote_ok(s.m2, &s.request_id, &s.leg_id, M1_PRICE, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await;

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let presented = v.snapshot(&s.request_id).await;
    let quotes = presented["quotes"].as_array().unwrap();
    assert_eq!(quotes[0]["submitted_at"], quotes[1]["submitted_at"], "timestamps tie");
    assert_eq!(selections(&presented), vec![q1.clone()], "lower seq wins");
    assert_quote_states(&presented, &[(&q1, "selected"), (&q2, "live")]);
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_missing_party_header_is_401() {
    let v = TestVenue::new();
    let body = json!({ "legs": [leg("buy_yes", LEG_NOTIONAL)], "response_deadline": ts(v.at(RESPONSE_DEADLINE_SECS)) });
    let (status, json) = v.call(Method::POST, "/v1/requests", None, Some(body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
    assert_eq!(json["code"], "missing_party");
    let (status, _) = v.call(Method::POST, "/v1/requests/x/accept", Some(Uuid::new_v4()), None).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "header present → past the extractor");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_unknown_ids_are_404() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let ghost = Uuid::new_v4().to_string();
    assert_eq!(v.get_request(&ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.accept(s.requester, &ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.quote(s.m1, &ghost, &s.leg_id, M1_PRICE, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.quote(s.m1, &s.request_id, &ghost, M1_PRICE, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await.0, StatusCode::NOT_FOUND, "unknown leg");
    assert_eq!(v.cancel_quote(s.m1, &ghost).await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.resolve(v.oracle_party(), &ghost, "yes").await.0, StatusCode::NOT_FOUND);
    assert_eq!(v.balances(s.m1).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_invalid_body_rejected() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let deadline = v.at(RESPONSE_DEADLINE_SECS);

    for (price, label) in [(0, "0%"), (10_000, "100%")] {
        let (status, body) = v.quote(s.m1, &s.request_id, &s.leg_id, price, LEG_NOTIONAL, v.at(QUOTE_EXPIRY_SECS)).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_price")), "price {label}");
    }
    let (status, body) = v.open_request(s.requester, json!([leg("buy_yes", 0)]), deadline).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("zero_notional")));
    let (status, body) = v.open_request(s.requester, json!([]), deadline).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("empty_legs")));
    let mut blank = leg("buy_yes", LEG_NOTIONAL);
    blank["contract"] = json!("   ");
    let (status, body) = v.open_request(s.requester, json!([blank]), deadline).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_contract_id")));

    assert_eq!(v.balances(s.m1).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_response_deadline_in_past_rejected() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    for (deadline, label) in [(v.at(-1), "past"), (v.now(), "now")] {
        let (status, body) = v.open_request(requester, json!([leg("buy_yes", LEG_NOTIONAL)]), deadline).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_eq!(body["code"], "deadline_in_past");
    }
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_response_deadline_beyond_max_rejected() {
    let v = TestVenue::new();
    let requester = Uuid::new_v4();
    let far_future = v.at(10 * 365 * 24 * 3_600);
    let (status, body) = v.open_request(requester, json!([leg("buy_yes", LEG_NOTIONAL)]), far_future).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a deadline years out must be rejected: {body}");
    v.assert_conserved().await;
}

// =============================================================================================
// Resolution
// =============================================================================================

#[tokio::test]
async fn fm_resolve_invalid_unwinds_refunds_each_side() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    // Only M2 quotes, at 60%: R (Yes-buyer) locks 600, M2 (Yes-seller) locks 400.
    let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    v.assert_balances(&[("requester", s.requester, bal(0, 0, 600)), ("m2", s.m2, bal(SIDE_LOCK - M2_LOCK, 0, M2_LOCK))]).await;

    let (status, unwound) = v.resolve(v.oracle_party(), &s.request_id, "invalid").await;
    assert_eq!(status, StatusCode::OK, "{unwound}");
    assert_eq!(unwound["state"], "unwound");
    assert_quote_states(&unwound, &[(&q2, "locked")]);
    v.assert_balances(&[
        ("requester", s.requester, bal(600, 0, 0)), // p·n back, not n/2
        ("m2", s.m2, bal(SIDE_LOCK, 0, 0)),          // n − p·n back
    ])
    .await;
    assert_eq!(v.resolve(v.oracle_party(), &s.request_id, "yes").await.0, StatusCode::CONFLICT, "terminal");
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_disputed_then_yes_pays_out() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);

    let (status, disputed) = v.resolve(v.oracle_party(), &s.request_id, "disputed").await;
    assert_eq!(status, StatusCode::OK, "{disputed}");
    assert_eq!(disputed["state"], "disputed");
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK), "still held, no payout");
    v.assert_conserved().await;

    let (status, settled) = v.resolve(v.oracle_party(), &s.request_id, "yes").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    assert_eq!(v.balances(s.requester).await, bal(3 * LEG_NOTIONAL, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, 0));
    }
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_resolve_before_locked_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    assert_eq!(v.resolve(v.oracle_party(), &s.request_id, "yes").await.0, StatusCode::CONFLICT, "while Open");
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let (status, body) = v.resolve(v.oracle_party(), &s.request_id, "yes").await;
    assert_eq!(status, StatusCode::CONFLICT, "while Presented: {body}");
    assert_eq!(body["code"], "wrong_state");
    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented");
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0), "no payout without escrow");
    v.assert_conserved().await;
}
