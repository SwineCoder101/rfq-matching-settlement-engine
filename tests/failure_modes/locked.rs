//! Presented → Locked: one atomic lock batch, and what may follow it.

use axum::http::StatusCode;
use rfq_matching_settlement_engine::domain::Amount;
use rfq_matching_settlement_engine::domain::PartyId;
use rfq_matching_settlement_engine::ledger::InsufficientFunds;
use rfq_matching_settlement_engine::ledger::Ledger;
use rfq_matching_settlement_engine::ledger::LockBatchError;
use rfq_matching_settlement_engine::ledger::LockItem;
use serde_json::json;

use super::scenarios::*;

#[tokio::test]
async fn fm_requester_insufficient_funds_at_accept() {
    let v = TestVenue::new();
    let s = v
        .three_leg_scenario_with_requester_funds(3 * SIDE_LOCK - 1)
        .await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;

    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["code"], "insufficient_funds");

    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["fail_reason"],
        json!({ "reason": "insufficient_requester_funds" })
    );
    assert_eq!(failed["escrows"], json!([]));
    for q in &quotes {
        assert_quote_states(&failed, &[(q, "released")]);
    }
    assert_eq!(
        v.balances(s.requester).await,
        bal(3 * SIDE_LOCK - 1, 0, 0),
        "failed batch mutated nothing"
    );
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
            LockItem::FromFree {
                party: b,
                amount: Amount::new(60),
            },
            LockItem::FromFree {
                party: c,
                amount: Amount::new(50),
            }, // short by 40
        ])
        .unwrap_err();
    assert_eq!(
        err,
        LockBatchError::InsufficientFunds(InsufficientFunds {
            party: c,
            needed: Amount::new(50),
            available: Amount::new(10)
        })
    );
    assert_eq!(
        [ledger.balance(a), ledger.balance(b), ledger.balance(c)],
        before,
        "first two items rolled back"
    );
    assert_eq!(ledger.escrowed_total(), Amount::ZERO);
    ledger.release(reservation);
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
    assert_eq!(
        v.balances(s.requester).await,
        bal(0, 0, 3 * SIDE_LOCK),
        "second accept locked nothing extra"
    );
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_accept_after_terminal_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    v.resolve_final(&s.request_id, "yes").await;
    let after_settle = v.balances(s.requester).await;

    assert_eq!(
        v.accept(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        v.reject(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        v.resolve(&s.request_id, "no").await.0,
        StatusCode::CONFLICT,
        "no second payout"
    );
    assert_eq!(v.snapshot(&s.request_id).await["state"], "settled");
    assert_eq!(v.balances(s.requester).await, after_settle);
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_resolve_before_locked_is_409() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    assert_eq!(
        v.resolve(&s.request_id, "yes").await.0,
        StatusCode::CONFLICT,
        "while Open"
    );
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let (status, body) = v.resolve(&s.request_id, "yes").await;
    assert_eq!(status, StatusCode::CONFLICT, "while Presented: {body}");
    assert_eq!(body["code"], "wrong_state");
    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented");
    assert_eq!(
        v.balances(s.requester).await,
        bal(3 * SIDE_LOCK, 0, 0),
        "no payout without escrow"
    );
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_resolve_invalid_unwinds_refunds_each_side() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    // Only M2 quotes, at 60%: R (Yes-buyer) locks 600, M2 (Yes-seller) locks 400.
    let q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    v.assert_balances(&[
        ("requester", s.requester, bal(0, 0, 600)),
        ("m2", s.m2, bal(SIDE_LOCK - M2_LOCK, 0, M2_LOCK)),
    ])
    .await;

    let (status, unwound) = v.resolve(&s.request_id, "invalid").await;
    assert_eq!(status, StatusCode::OK, "{unwound}");
    assert_eq!(unwound["state"], "unwound");
    assert_quote_states(&unwound, &[(&q2, "locked")]);
    v.assert_balances(&[
        ("requester", s.requester, bal(600, 0, 0)), // p·n back, not n/2
        ("m2", s.m2, bal(SIDE_LOCK, 0, 0)),         // n − p·n back
    ])
    .await;
    assert_eq!(
        v.resolve(&s.request_id, "yes").await.0,
        StatusCode::CONFLICT,
        "terminal"
    );
    v.assert_conserved().await;
}
