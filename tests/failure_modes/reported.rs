//! Locked → Reported: the oracle's Yes/No is held for the dispute window before it pays out.

use axum::http::StatusCode;
use uuid::Uuid;

use super::scenarios::*;

/// A report starts the clock and moves nothing.
#[tokio::test]
async fn fm_report_does_not_pay_out() {
    let v = TestVenue::new();
    let (s, reported_at) = reported_yes(&v).await;
    let r = v.snapshot(&s.request_id).await;
    assert_eq!(r["state"], "reported");
    assert_eq!(r["reported_outcome"], "yes");
    assert_eq!(
        r["dispute_deadline"],
        ts(v.at(reported_at + DISPUTE_WINDOW_SECS))
    );
    assert_eq!(
        r["escrows"].as_array().unwrap().len(),
        3,
        "escrow untouched"
    );
    escrowed_three(&v, &s).await;
    v.assert_conserved().await;
}

/// No filing inside the window: the reported outcome settles on the first tick past it.
/// Filing is still allowed at the deadline instant itself.
#[tokio::test]
async fn fm_unfiled_report_settles_after_window() {
    let v = TestVenue::new();
    let (s, reported_at) = reported_yes(&v).await;

    v.advance_to(reported_at + DISPUTE_WINDOW_SECS).await;
    assert_eq!(
        v.snapshot(&s.request_id).await["state"],
        "reported",
        "still disputable at the deadline instant"
    );
    escrowed_three(&v, &s).await;

    v.advance_to(reported_at + DISPUTE_WINDOW_SECS + 1).await;
    let settled = v.snapshot(&s.request_id).await;
    assert_eq!(settled["state"], "settled");
    assert_eq!(
        v.balances(s.requester).await,
        bal(3 * LEG_NOTIONAL, 0, 0),
        "Yes-buyer receives n per leg, once"
    );
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, 0));
    }
    let (status, body) = v.dispute(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "too late to file: {body}");
    assert_eq!(v.balances(s.requester).await, bal(3 * LEG_NOTIONAL, 0, 0));
    v.assert_conserved().await;
}

/// Only parties to the request may file: a stranger and a maker whose quote lost are 403.
#[tokio::test]
async fn fm_stranger_cannot_dispute() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    let _q1 = s.quote_m1(&v, QUOTE_EXPIRY_SECS).await; // wins
    let _q2 = s.quote_m2(&v, QUOTE_EXPIRY_SECS).await; // loses, released at accept
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    assert_eq!(v.resolve(&s.request_id, "yes").await.0, StatusCode::OK);

    for (label, party) in [("stranger", Uuid::new_v4()), ("losing maker", s.m2)] {
        let (status, body) = v.dispute(party, &s.request_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {body}");
        assert_eq!(body["code"], "not_owner");
    }
    assert_eq!(v.snapshot(&s.request_id).await["state"], "reported");
    assert_eq!(v.balances(s.requester).await, bal(100, 0, 500));
    assert_eq!(v.balances(s.m1).await, bal(0, 0, SIDE_LOCK));
    assert_eq!(v.balances(s.m2).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

/// Filing is only possible while Reported, and a report cannot be overwritten: the oracle
/// changes its mind only through a dispute.
#[tokio::test]
async fn fm_dispute_only_while_reported() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT,
        "Open"
    );
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT,
        "Presented"
    );
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT,
        "Locked"
    );

    assert_eq!(v.resolve(&s.request_id, "yes").await.0, StatusCode::OK);
    let (status, body) = v.resolve(&s.request_id, "no").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a report is not overwritten: {body}"
    );
    assert_eq!(v.snapshot(&s.request_id).await["reported_outcome"], "yes");
    escrowed_three(&v, &s).await;
    v.assert_conserved().await;
}
