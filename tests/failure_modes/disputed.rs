//! Disputed: filed by a party or posted by the oracle; adjudication or the unwind timeout is the only way out.

use axum::http::StatusCode;

use super::scenarios::*;

#[tokio::test]
async fn fm_disputed_then_yes_pays_out() {
    let v = TestVenue::new();
    let s = v.three_leg_scenario().await;
    v.quote_all_legs(&s).await;
    v.advance_to(RESPONSE_DEADLINE_SECS).await;
    assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);

    let (status, disputed) = v.resolve(&s.request_id, "disputed").await;
    assert_eq!(status, StatusCode::OK, "{disputed}");
    assert_eq!(disputed["state"], "disputed");
    assert_eq!(
        v.balances(s.requester).await,
        bal(0, 0, 3 * SIDE_LOCK),
        "still held, no payout"
    );
    v.assert_conserved().await;

    let (status, settled) = v.resolve(&s.request_id, "yes").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    assert_eq!(v.balances(s.requester).await, bal(3 * LEG_NOTIONAL, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, 0));
    }
    v.assert_conserved().await;
}

/// REVIEW #7. Every exit from Disputed other than `yes` (that is R2): a repeated `disputed`
/// changes nothing, `no` pays each leg to its Yes-seller, `invalid` refunds each poster its
/// own chunk. Three legs, all `buy_yes`, so the requester is the Yes-buyer on every leg.
#[tokio::test]
async fn fm_disputed_exits() {
    async fn locked_then_disputed(v: &TestVenue) -> ThreeLeg {
        let s = v.three_leg_scenario().await;
        v.quote_all_legs(&s).await;
        v.advance_to(RESPONSE_DEADLINE_SECS).await;
        assert_eq!(v.accept(s.requester, &s.request_id).await.0, StatusCode::OK);
        let (status, disputed) = v.resolve(&s.request_id, "disputed").await;
        assert_eq!(status, StatusCode::OK, "{disputed}");
        assert_eq!(disputed["state"], "disputed");
        s
    }

    // (a) disputed again: 200, still disputed, ledger untouched.
    let v = TestVenue::new();
    let s = locked_then_disputed(&v).await;
    let (status, again) = v.resolve(&s.request_id, "disputed").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["state"], "disputed");
    assert_eq!(
        v.balances(s.requester).await,
        bal(0, 0, 3 * SIDE_LOCK),
        "still held"
    );
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(0, 0, SIDE_LOCK));
    }
    v.assert_conserved().await;

    // (b) no: each leg's Yes-seller (the maker) receives both chunks, n in total.
    let v = TestVenue::new();
    let s = locked_then_disputed(&v).await;
    let (status, settled) = v.resolve(&s.request_id, "no").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    assert_eq!(
        v.balances(s.requester).await,
        bal(0, 0, 0),
        "Yes-buyer loses its p·n on every leg"
    );
    for m in s.makers {
        assert_eq!(
            v.balances(m).await,
            bal(LEG_NOTIONAL, 0, 0),
            "Yes-seller receives n"
        );
    }
    v.assert_conserved().await;

    // (c) invalid: each poster gets its own chunk back; nobody profits; terminal.
    let v = TestVenue::new();
    let s = locked_then_disputed(&v).await;
    let (status, unwound) = v.resolve(&s.request_id, "invalid").await;
    assert_eq!(status, StatusCode::OK, "{unwound}");
    assert_eq!(unwound["state"], "unwound");
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(
        v.resolve(&s.request_id, "yes").await.0,
        StatusCode::CONFLICT,
        "terminal"
    );
    v.assert_conserved().await;
}

/// A party's filing holds escrow: the request is Disputed, the old window no longer settles
/// it, an unwind deadline is set, and nothing moves. Requester and locked maker may both file.
#[tokio::test]
async fn fm_party_dispute_holds_escrow() {
    for filer in ["requester", "maker"] {
        let v = TestVenue::new();
        let (s, reported_at) = reported_yes(&v).await;
        let party = if filer == "requester" {
            s.requester
        } else {
            s.makers[1]
        };

        let filed_at = reported_at + 10;
        v.set(v.at(filed_at));
        let (status, body) = v.dispute(party, &s.request_id).await;
        assert_eq!(status, StatusCode::OK, "{filer}: {body}");
        assert_eq!(body["state"], "disputed");
        assert_eq!(
            body["unwind_deadline"],
            ts(v.at(filed_at + UNWIND_TIMEOUT_SECS)),
            "{filer}"
        );
        escrowed_three(&v, &s).await;

        // The report's window passing changes nothing once disputed.
        v.advance_to(reported_at + DISPUTE_WINDOW_SECS + 1).await;
        assert_eq!(
            v.snapshot(&s.request_id).await["state"],
            "disputed",
            "{filer}"
        );
        escrowed_three(&v, &s).await;

        // Filing twice is a 409, not a second hold.
        assert_eq!(
            v.dispute(party, &s.request_id).await.0,
            StatusCode::CONFLICT,
            "{filer}"
        );
        v.assert_conserved().await;
    }
}

/// Adjudication is final and immediate: `no` pays the Yes-sellers, `invalid` refunds each
/// poster, and either way a second resolve is 409.
#[tokio::test]
async fn fm_adjudication_settles_or_unwinds_once() {
    // no: reverses the reported yes, makers (Yes-sellers) receive n each.
    let v = TestVenue::new();
    let (s, _) = reported_yes(&v).await;
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::OK
    );
    let (status, settled) = v.resolve(&s.request_id, "no").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "settled");
    assert_eq!(v.balances(s.requester).await, bal(0, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(LEG_NOTIONAL, 0, 0));
    }
    assert_eq!(
        v.resolve(&s.request_id, "yes").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::CONFLICT
    );
    v.assert_conserved().await;

    // invalid: each poster gets its own chunk back.
    let v = TestVenue::new();
    let (s, _) = reported_yes(&v).await;
    assert_eq!(
        v.dispute(s.makers[0], &s.request_id).await.0,
        StatusCode::OK
    );
    let (status, unwound) = v.resolve(&s.request_id, "invalid").await;
    assert_eq!(status, StatusCode::OK, "{unwound}");
    assert_eq!(unwound["state"], "unwound");
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(v.resolve(&s.request_id, "no").await.0, StatusCode::CONFLICT);
    v.assert_conserved().await;
}

/// No adjudication before the unwind timeout: everyone gets their own money back, once.
#[tokio::test]
async fn fm_unwind_timeout_refunds_each_poster() {
    let v = TestVenue::new();
    let (s, reported_at) = reported_yes(&v).await;
    let filed_at = reported_at + 10;
    v.set(v.at(filed_at));
    assert_eq!(
        v.dispute(s.requester, &s.request_id).await.0,
        StatusCode::OK
    );

    v.advance_to(filed_at + UNWIND_TIMEOUT_SECS).await;
    assert_eq!(
        v.snapshot(&s.request_id).await["state"],
        "disputed",
        "still adjudicable at the deadline instant"
    );
    escrowed_three(&v, &s).await;

    v.advance_to(filed_at + UNWIND_TIMEOUT_SECS + 1).await;
    let unwound = v.snapshot(&s.request_id).await;
    assert_eq!(unwound["state"], "unwound");
    assert_eq!(v.balances(s.requester).await, bal(3 * SIDE_LOCK, 0, 0));
    for m in s.makers {
        assert_eq!(v.balances(m).await, bal(SIDE_LOCK, 0, 0));
    }
    assert_eq!(
        v.resolve(&s.request_id, "yes").await.0,
        StatusCode::CONFLICT,
        "terminal"
    );
    v.assert_conserved().await;
}
