//! Open → Presented at the response deadline, and the accept window. A match is a reservation, never a lock, until the requester accepts.

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use super::scenarios::*;

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
    assert_eq!(
        failed["fail_reason"],
        json!({ "reason": "leg_unmatched", "leg_id": s.leg_ids[1] })
    );
    assert!(
        failed.get("package").is_none(),
        "requester never sees a partial package"
    );
    assert_eq!(failed["escrows"], json!([]));
    assert_quote_states(&failed, &[(&q0, "released"), (&q2, "released")]);

    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    assert_eq!(
        v.ledger.lock_batch_calls(),
        0,
        "lock_batch is never called on a failed match"
    );
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
    assert_eq!(
        presented["accept_deadline"],
        ts(v.at(ACCEPT_DEADLINE_SECS + 1))
    );
    assert_eq!(
        selections(&presented),
        vec![q2.clone()],
        "worse but longer-lived quote wins"
    );
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
    v.assert_balances(&[
        ("m1", s.m1, bal(SIDE_LOCK, 0, 0)),
        ("m2", s.m2, bal(SIDE_LOCK - M2_LOCK, M2_LOCK, 0)),
    ])
    .await;
    v.assert_conserved().await;
}

#[tokio::test]
async fn fm_tie_breaks_on_seq() {
    let v = TestVenue::new();
    let s = one_leg(&v).await;
    // Same price, same frozen clock instant; M2 submits second.
    let q1 = s.quote_m1(&v, QUOTE_EXPIRY_SECS).await;
    let q2 = v
        .quote_ok(
            s.m2,
            &s.request_id,
            &s.leg_id,
            M1_PRICE,
            LEG_NOTIONAL,
            v.at(QUOTE_EXPIRY_SECS),
        )
        .await;

    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    let presented = v.snapshot(&s.request_id).await;
    let quotes = presented["quotes"].as_array().unwrap();
    assert_eq!(
        quotes[0]["submitted_at"], quotes[1]["submitted_at"],
        "timestamps tie"
    );
    assert_eq!(selections(&presented), vec![q1.clone()], "lower seq wins");
    assert_quote_states(&presented, &[(&q1, "selected"), (&q2, "live")]);
    v.assert_conserved().await;
}

/// The accept window never outlives the contracts: a worker so late that `resolves_at` has
/// passed presents a package the requester can no longer accept, and everyone is released.
#[tokio::test]
async fn fm_accept_window_capped_at_resolution() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    assert_eq!(
        v.snapshot(&s.request_id).await["resolves_at"],
        ts(v.at(RESPONSE_DEADLINE_SECS + super::common::TENOR_SECS))
    );

    // Worker wakes up one second after the contracts resolved.
    v.advance_to(RESPONSE_DEADLINE_SECS + super::common::TENOR_SECS + 1)
        .await;
    let presented = v.snapshot(&s.request_id).await;
    assert_eq!(presented["state"], "presented");
    assert_eq!(
        presented["accept_deadline"],
        ts(v.at(RESPONSE_DEADLINE_SECS + super::common::TENOR_SECS)),
        "window ends at resolves_at, not now + accept_window"
    );

    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["fail_reason"],
        json!({ "reason": "accept_window_expired" })
    );
    for q in &quotes {
        assert_quote_states(&failed, &[(q, "released")]);
    }
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.ledger.lock_batch_calls(), 0);
    v.assert_conserved().await;
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
        assert_eq!(
            r["state"], "presented",
            "M2 keeps the leg matched either way"
        );
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
    assert!(
        cancelled > 0 && selected > 0,
        "both interleavings exercised: {cancelled} cancelled, {selected} selected"
    );
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
    assert_quote_states(
        &v.snapshot(&s.request_id).await,
        &[(&quotes[0], "selected")],
    );
    assert_eq!(
        v.balances(s.makers[0]).await,
        bal(0, SIDE_LOCK, 0),
        "reservation intact"
    );
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

    let (status, body) = v
        .quote(
            late_maker,
            &s.request_id,
            &s.leg_ids[0],
            LEG_PRICE_BPS - 1,
            LEG_NOTIONAL,
            v.at(QUOTE_EXPIRY_SECS),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    assert_eq!(
        v.snapshot(&s.request_id).await["quotes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(v.balances(late_maker).await, bal(SIDE_LOCK, 0, 0));
    v.assert_conserved().await;
}

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
    assert_eq!(
        v.snapshot(&s.request_id).await["state"],
        "presented",
        "accept is allowed at the deadline instant"
    );

    v.advance_to(ACCEPT_DEADLINE_SECS + 1).await;
    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["fail_reason"],
        json!({ "reason": "accept_window_expired" })
    );
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

/// REVIEW #3. The worker is not the only guard on the accept window. An accept that arrives
/// after `accept_deadline` with no `Tick` in between must itself fail the request and hand
/// every maker its collateral back; at the deadline instant itself, accept still succeeds.
#[tokio::test]
async fn fm_accept_past_deadline_without_tick_fails_request() {
    // One second past the deadline, no tick delivered.
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.snapshot(&s.request_id).await["state"], "presented");

    v.set(v.at(ACCEPT_DEADLINE_SECS + 1));
    let (status, body) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "wrong_state");
    let failed = v.snapshot(&s.request_id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["fail_reason"],
        json!({ "reason": "accept_window_expired" })
    );
    assert_eq!(failed["escrows"], json!([]));
    for q in &quotes {
        assert_quote_states(&failed, &[(q, "released")]);
    }
    assert_eq!(
        v.balances(s.requester).await,
        bal(3 * SIDE_LOCK, 0, 0),
        "nothing locked"
    );
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.ledger.lock_batch_calls(), 0, "no batch attempted");
    v.assert_conserved().await;

    // Exactly at the deadline, no tick delivered: accept is still allowed.
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    let quotes = v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;

    v.set(v.at(ACCEPT_DEADLINE_SECS));
    let (status, locked) = v.accept(s.requester, &s.request_id).await;
    assert_eq!(status, StatusCode::OK, "{locked}");
    assert_eq!(locked["state"], "locked");
    for q in &quotes {
        assert_quote_states(&locked, &[(q, "locked")]);
    }
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 3 * SIDE_LOCK));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, SIDE_LOCK));
    }
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
                assert_eq!(
                    r["fail_reason"],
                    json!({ "reason": "accept_window_expired" })
                );
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
    assert!(
        locked_wins > 0 && expiry_wins > 0,
        "both interleavings exercised: {locked_wins} locked, {expiry_wins} expired"
    );
}
