# Verification pass

Read-only. No source, test, or doc was modified. Every claim below cites `file:line` in the working tree as of this pass (post-`cargo fmt`). Probes that no repo test covers were run against a scratchpad copy of the repo with an extra test file; their results are quoted, and their code is not in the repo.

No exercise brief is present in the repository; the docs and the scope list given in-session were used instead.

## 0. Baseline

`cargo test` (full run, no filters):

| Suite | Result |
|---|---|
| unit (`src/lib.rs`) | 42 passed, 0 failed, 0 ignored |
| `tests/failure_modes.rs` | 28 passed, 0 failed, **1 ignored** (`fm_self_quote_rejected`) |
| `tests/happy_path.rs` | 3 passed |
| `tests/settlement.rs` | 14 passed |
| doc-tests | 0 |

`cargo test --test failure_modes -- --ignored`: `fm_self_quote_rejected` FAILED at `tests/failure_modes.rs:650` (`left: 201, right: 400`). It is the only red test in the suite, and it is red only when explicitly requested; the default run is fully green.

<details><summary>Complete test list</summary>

```
domain::ids::tests::seq_is_monotonic ok
domain::ids::tests::contract_description_is_trimmed_and_bounded ok
domain::ids::tests::contract_id_rejects_empty_and_whitespace ok
domain::money::tests::amount_arithmetic ok
domain::money::tests::buyer_lock_is_truncated_share_of_notional ok
domain::money::tests::buyer_plus_seller_equals_notional_for_sweep ok
domain::money::tests::price_rejects_zero_and_full ok
domain::money::tests::amount_sub_underflow_panics - should panic ok
domain::request::tests::escrow_roles_follow_leg_side ok
domain::request::tests::leg_rejects_zero_notional ok
domain::request::tests::open_request_starts_clean ok
domain::request::tests::maker_lock_is_the_mm_side_of_escrow ok
domain::request::tests::request_rejects_empty_legs ok
domain::request::tests::request_serializes_as_the_wire_shape ok
domain::state::tests::fail_reason_serializes_with_tag ok
domain::state::tests::leg_side_round_trips_through_snake_case_wire_names ok
domain::state::tests::leg_side_role ok
clock::tests::mock_clock_only_moves_when_told ok
api::tests::engine_errors_map_to_expected_statuses ok
api::tests::api_errors_are_bad_request ok
api::tests::zero_amount_and_zero_size_are_bad_request ok
api::tests::leg_body_parses_into_domain_leg ok
api::tests::error_body_carries_code_and_message ok
api::tests::resolve_body_parses_outcome ok
api::tests::balance_view_flattens_account ok
api::tests::leg_body_rejects_bad_input ok
ledger::tests::credit_and_balance ok
ledger::tests::ledger_account_total ok
ledger::tests::happy_path_lock_payout_and_refund ok
ledger::tests::lock_batch_is_all_or_nothing ok
ledger::tests::lock_batch_rejects_unknown_and_reused_reservations ok
ledger::tests::lock_batch_sums_multiple_free_items_for_one_party ok
ledger::tests::reserve_then_release_round_trips ok
matching::tests::all_expired_has_no_best ok
matching::tests::empty_has_no_best ok
matching::tests::expiring_inside_accept_window_is_ineligible ok
matching::tests::long_yes_sides_take_lowest_yes_price ok
matching::tests::non_live_quotes_are_ineligible ok
matching::tests::quotes_on_other_legs_are_ignored ok
matching::tests::price_tie_breaks_on_lowest_seq_not_submitted_at ok
matching::tests::short_yes_sides_take_highest_yes_price ok
matching::tests::size_too_small_is_ineligible ok
fm_lock_batch_is_atomic ok
fm_missing_party_header_is_401 ok
fm_maker_insufficient_funds_at_quote_is_402 ok
fm_invalid_body_rejected ok
fm_expired_quote_not_selected_and_released ok
fm_cancel_released_quote_is_409 ok
fm_maker_cannot_cancel_others_quote ok
fm_leg_unmatched_at_deadline_releases_all ok
fm_non_owner_cannot_accept_or_reject ok
fm_double_accept_is_409 ok
fm_quote_too_small_rejected ok
fm_accept_window_expiry_fails_request ok
fm_accept_after_terminal_is_409 ok
fm_self_quote_rejected ignored, known gap: engine does not yet reject self-quoting at request open
fm_quote_after_presented_is_409 ok
fm_quote_expiring_before_accept_deadline_is_ineligible ok
fm_quote_expiring_before_accept_window_rejected_at_submit ok
fm_response_deadline_in_past_rejected ok
fm_disputed_then_yes_pays_out ok
fm_requester_insufficient_funds_at_accept ok
fm_requester_reject_releases_selected_and_losers ok
fm_resolve_before_locked_is_409 ok
fm_happy_path_three_legs ok
fm_tie_breaks_on_seq ok
fm_resolve_invalid_unwinds_refunds_each_side ok
fm_unknown_ids_are_404 ok
fm_selected_quote_cannot_be_cancelled ok
fm_cancel_and_tick_race ok
fm_accept_and_tick_race ok
missing_party_header_is_unauthorized ok
unmatched_leg_fails_request_and_releases_every_reservation ok
full_lifecycle_two_legs_settles_yes ok
settles::case_01_buy_yes_on_yes .. case_14_odd_notional_on_no ok (14)
```
</details>

Probe results (scratchpad copy, not in repo):

| Probe | Result |
|---|---|
| accept with clock exactly at `accept_deadline`, no tick | 200, `locked` |
| accept with clock 1s past `accept_deadline`, no tick | 409, `failed(accept_window_expired)`, makers released, conserved |
| `disputed` twice, then `invalid` | 200 `disputed` (no-op), then 200 `unwound`, all refunded, conserved |
| losing quote whose own `expires_at` passes while Presented | stays `live`, still reserved, cancel → 409 |
| open request with `response_deadline` = chrono `MAX_UTC − 10s`, then one quote | open → 201; quote → **panic at `src/engine.rs:298` "`DateTime + TimeDelta` overflowed"**; every later call → 503 `engine_unavailable` |

---

## Part 1 — Actor flow matrices

Column key: endpoint → extractor/authz → `Command` → engine arm → ledger calls in order → state after → tests.

### Requester

| Step | Endpoint | Extractor / authz | Command | Engine arm and guards | Ledger calls (order) | State after | Tests |
|---|---|---|---|---|---|---|---|
| Credit faucet | `POST /v1/ledger/credit` `api.rs:29`, handler `api.rs:228-241` | none (party in body); `amount == 0` → 400 `zero_amount` `api.rs:232-234` | `Credit` `engine.rs:135-139` | `engine.rs:244-251` | `credit` `ledger.rs:181-185` (free += amount) | n/a | `common/mod.rs:303-307` (`fund`), `happy_path.rs:22-30` |
| Open request | `POST /v1/requests` `api.rs:31`, handler `api.rs:255-270` | `Party` `api.rs:51-74` (401 `missing_party` if absent/non-UUID). Validation: legs non-empty `request.rs:156-158` via `engine.rs:270` → 400 `empty_legs` `api.rs:106`; notional > 0 `request.rs:36-38` via `api.rs:182-187` → 400 `zero_notional` `api.rs:142`; contract/description non-blank `ids.rs:54-60`, `ids.rs:87-97`; deadline future `engine.rs:266-269` (`response_deadline <= now` → 400 `deadline_in_past` `api.rs:105`). Price bounds are not a request field. | `SubmitRequest` `engine.rs:96-101` | `engine.rs:188-195` → `submit_request` `engine.rs:260-273` | none | `Open` `request.rs:166` | `failure_modes.rs:856-900` (P9), `903-914` (P10), `happy_path.rs:35-51` (201, exact body fixture) |
| Poll while Open | `GET /v1/requests/{id}` `api.rs:32`, handler `api.rs:272-277` | none | `GetRequest` `engine.rs:131-134` | `engine.rs:236-243` (404 if unknown) | none | unchanged | `happy_path.rs:85-88`, `failure_modes.rs:820` (404) |
| See package at Presented | same GET; transition is driven by `Tick` `engine.rs:146`, `engine.rs:255` → `tick` `engine.rs:496-513` → `present_or_fail` `engine.rs:607-636` using `select_best` `matching.rs:16-36` | n/a | `Tick` (from worker `worker.rs:15-23`, `main.rs:21`) | selected quotes → `Selected` `engine.rs:628-632`; `package` `engine.rs:633`; `accept_deadline = now + accept_window` `engine.rs:511`, `634` | none (present) / `release` per quote on fail `engine.rs:623` → `666` | `Presented` `engine.rs:635` | `happy_path.rs:91-120` (package fixture `tests/fixtures/responses/package_two_leg.json:1-6`, quote states, `accept_deadline`), `failure_modes.rs:112-115` |
| Response body shows selected quotes | `package.selections[{leg_id, quote_id}]` `request.rs:83-93`, `request.rs:140`; the selected `quotes[]` entries carry `price_bps` `request.rs:56-57` and `state: selected` | | | | | | `happy_path.rs:98-113` |
| Response body shows the amount accept will lock | **Not shown.** `escrows` is empty until `Locked` (`request.rs:141-142`, populated at `engine.rs:416`). The requester must derive `floor(p·n)` or `n − floor(p·n)` from the selected quote's `price_bps` and the leg's `side`/`notional` (`money.rs:112-122`, `state.rs:23-25`). | | | | | | none → **Part 4 gap** |
| Accept | `POST /v1/requests/{id}/accept` `api.rs:34`, handler `api.rs:312-320` | `Party`; ownership `engine.rs:358-360` → 403 `not_owner` `api.rs:93` | `Accept` `engine.rs:116-120` | `engine.rs:215-221` → `accept` `engine.rs:348-419`: 404 `354-357`; owner `358-360`; state `Presented` `361` (409); **deadline re-check** `362-379` (`now > accept_deadline` → `fail_request` → 409 with `actual: Failed`); `escrow_plan` `547-579` builds `[FromReservation(maker), FromFree(requester)]` per leg `571-575` | 1. `lock_batch` `engine.rs:382` → `ledger.rs:215-278` (phase 1 validate `225-255`, phase 2 apply `258-276`); 2. `release` for every still-`Live` quote `engine.rs:594-598` → `ledger.rs:205-213` | `Locked` `engine.rs:417`; selected quotes `Locked` `590-593`; losers `Released` `594-595`; `escrows` set `416`; handles mapped by role `397-414` | `happy_path.rs:123-147` (escrow fixture `tests/fixtures/responses/escrows_two_leg_locked.json`, quote states, balances), `failure_modes.rs:117-138` (H1), `600-618` (P1), `454-471` (A9), `502-549` (A3). **Deadline re-check `362-379`: no repo test** (probe passed) → Part 4 |
| Reject | `POST /v1/requests/{id}/reject` `api.rs:35`, handler `api.rs:322-330` | `Party`; ownership `engine.rs:431-433` → 403 | `Reject` `engine.rs:121-125` | `engine.rs:222-228` → `reject` `engine.rs:422-442`: 404, owner, state `Presented` `434` | `release` for every `Live`/`Selected` quote via `fail_request` `engine.rs:435-440` → `666` → `639-653` | `Failed(rejected)` `engine.rs:667-668` | `failure_modes.rs:278-299` (A1), `610-611` (P1) |
| Window expiry | none (worker) | n/a | `Tick` | `engine.rs:515-523` (`now > accept_deadline`) → `fail_request`; also the accept-side path `362-379` | `release` all holds `engine.rs:666` | `Failed(accept_window_expired)` | `failure_modes.rs:302-332` (A2), `502-549` (A3) |
| Insufficient funds at accept | accept | as accept | `Accept` | `engine.rs:382-392`: `lock_batch` returns `InsufficientFunds` from phase 1 (`ledger.rs:239-252`) having mutated nothing → `fail_request(InsufficientRequesterFunds)` `385-390` → `Err` → 402 `api.rs:96-98` | 1. `lock_batch` (refused, `lock_batch_calls` still incremented `ledger.rs:217`); 2. `release` all maker holds `engine.rs:666` | `Failed(insufficient_requester_funds)` | `failure_modes.rs:197-229` (M2), `232-271` (M3, ledger-level) |
| Resolved payout lands in free | `POST /v1/oracle/resolve` `api.rs:37`, handler `api.rs:332-341` | **no extractor, no authz** | `Resolve` `engine.rs:126-130` | `engine.rs:229-235` → `resolve` `engine.rs:445-491`: state `Locked|Disputed` `454-459` else 409; winner = `yes_buyer` on Yes, `yes_seller` on No `467-471` | `payout(yes_buyer_handle, winner)`, `payout(yes_seller_handle, winner)` per leg `engine.rs:472-473` → `ledger.rs:280-294` (`escrowed -=` poster, `free +=` winner `285-286`) | `Settled` `engine.rs:475` | `happy_path.rs:150-158` (requester 8_950 → 9_950), `failure_modes.rs:140-150` (H1), `settlement.rs:150-160` |

### Market maker

| Step | Endpoint | Extractor / authz | Command | Engine arm and guards | Ledger calls (order) | State after | Tests |
|---|---|---|---|---|---|---|---|
| Credit | as requester row 1 | | | | | | |
| Submit quote | `POST /v1/requests/{id}/quotes` `api.rs:33`, handler `api.rs:279-301` | `Party` `api.rs:51-74`; `size == 0` → 400 `zero_size` `api.rs:285-287`; price bounds `Price::new` `api.rs:288` → `money.rs:98-103` (`1..=9_999`) → 400 `invalid_price` `api.rs:139` | `SubmitQuote` `engine.rs:102-110` | `engine.rs:196-207` → `submit_quote` `engine.rs:276-320`: 404 request `286-289` / leg `291`; request `Open` `290` (409); **self-quote guard: absent** (no `maker != req.requester` check anywhere in `276-320`); `expires_at <= now` → 400 `quote_expired` `292-294`; `size < notional` → 400 `quote_too_small` `295-297`; `expires_at < response_deadline + accept_window` → 400 `quote_expires_before_accept_window` `298-300` | `reserve(maker, quote.maker_lock(leg))` `engine.rs:313` → `ledger.rs:187-203` (free → reserved; short → `InsufficientFunds` → 402 `api.rs:96`). Amount per side: `request.rs:70-80`: requester long Yes (`buy_yes`/`sell_no`) → maker is **Yes-seller** → `yes_seller_lock` = `n − floor(p·n)` `money.rs:120-122`; requester short Yes (`sell_yes`/`buy_no`) → maker is **Yes-buyer** → `yes_buyer_lock` = `floor(p·n)` `money.rs:112-116` | quote `Live` `engine.rs:311`, `seq` stamped `310`/`315`, reservation recorded `316`, owner map `317` | `happy_path.rs:56-72` (leg A `buy_yes`: reserves 600/650 = `(1−p)n`; leg B `sell_yes`: reserves 1_200/1_300 = `p·n`; size 2_500 > notional 2_000 reserves only `p·n`), `failure_modes.rs:357-395` (A6), `665-686` (P3), `689-707` (P4), `710-739` (P5), `861-877` (price bounds); unit `request.rs:292` |
| Cancel while Live | `DELETE /v1/quotes/{id}` `api.rs:36`, handler `api.rs:303-310` | `Party`; quote owner `engine.rs:333-335` → 403 `not_owner` | `CancelQuote` `engine.rs:111-115` | `engine.rs:208-214` → `cancel_quote` `engine.rs:323-344`: 404 `324-332`; owner `333-335`; request `Open` `336` (409 `wrong_state`); quote `Live` `337-339` (409 `quote_not_live`) | `release` `engine.rs:340-342` → `648-649` → `ledger.rs:205-213` | quote `Released` `engine.rs:647`; 204 `api.rs:309` | `happy_path.rs:75-88`, `failure_modes.rs:742-761` (P6), `621-632` (P2), `552-593` (A4) |
| Cancel refused once Selected | same | | | refused by the **request-state** check `engine.rs:336` (request is `Presented`) → 409 `wrong_state`; the quote-state check `337-339` is never reached for a `Selected` quote | none | unchanged, reservation intact | `failure_modes.rs:335-354` (A5), `580-584` (A4 lost-race branch) |
| Released on lose / reject / expiry / fail | n/a | n/a | `Accept` / `Reject` / `Tick` | lose at accept `engine.rs:594-598`; reject `435-440` → `666`; accept-window expiry `521` and `369-374` → `666`; leg unmatched `621-625` → `666`; requester short at accept `385-390` → `666`; own expiry while `Open` `502-504` | `release` `ledger.rs:205-213` per quote | quote `Released` | `happy_path.rs:133-147`, `failure_modes.rs:278-299` (A1), `302-332` (A2), `162-194` (M1), `197-229` (M2), `433-451` (A8) |
| Reservation converted at accept | n/a | n/a | `Accept` | `LockItem::FromReservation` `engine.rs:571` → `ledger.rs:226-238` (validated), `261-266` (reserved → escrowed, reservation consumed); engine `Selected → Locked` and drops its map entry `engine.rs:590-593` | inside `lock_batch` | quote `Locked` | `happy_path.rs:142-147` (mm2 1_950 reserved → escrowed), `failure_modes.rs:134-137`; unit `ledger.rs:473-510` |
| Paid `n` on win | resolve (by anyone) | none | `Resolve` | maker wins when (Yes ∧ maker is `yes_buyer`, i.e. `sell_yes`/`buy_no` legs) or (No ∧ maker is `yes_seller`, i.e. `buy_yes`/`sell_no` legs) `engine.rs:467-471`; both handles paid to winner `472-473` | `payout` ×2 `ledger.rs:280-294` | `Settled` | `settlement.rs:29` (`buy_yes_on_no`: maker Yes-seller wins), `:30` (`sell_yes_on_yes`: maker Yes-buyer wins), `:32`, `:35`; winner balance `settlement.rs:155-160`; `happy_path.rs:153-158` (mm2 as Yes-buyer on leg B receives 2_000) |
| Stake paid to winner on loss | resolve | none | `Resolve` | maker's own chunk goes to `winner` `engine.rs:472-473` | `payout` | `Settled` | `settlement.rs:28` (`buy_yes_on_yes`: maker −650), `happy_path.rs:153-158` (mm2 loses leg A), `failure_modes.rs:143-150` (H1: makers end at 0) |
| Refunded on Invalid | resolve | none | `Resolve` | `engine.rs:477-486` | `refund` ×2 per leg `ledger.rs:296-304` (escrowed → free of the poster) | `Unwound` `engine.rs:486` | `failure_modes.rs:921-949` (R1: m2 gets its 400 back, `:940`) |

**Maker-as-Yes-buyer (`sell_yes` / `buy_no`).** The per-side reserve amount is `request.rs:75-79`; escrow roles are `request.rs:109-113`; the handle-to-role mapping at accept is `engine.rs:402-412`. This case **is** covered on the wire: `happy_path.rs:64-72` asserts the `p·n` reservation on a `sell_yes` leg, `tests/fixtures/responses/escrows_two_leg_locked.json:10-17` pins the maker as `yes_buyer` with `1300/700`, and `settlement.rs:30-33, 37-48` run every maker-buy-side case through accept and both outcomes with exact escrow JSON (`settlement.rs:102-112, 132`). What is *not* covered: `tests/failure_modes.rs` uses only `buy_yes` legs (`common/mod.rs:459-462`, `failure_modes.rs:46`), so every failure and race path runs with the maker as Yes-seller. Release logic is side-agnostic (it acts on reservation handles, `engine.rs:639-653`), so this is a low-value hole rather than a doc problem; noted in Part 4.

**(a) Requester journey.** Every step is reachable with documented endpoints only: `POST /v1/ledger/credit`, `POST /v1/requests`, `GET /v1/requests/{id}`, `POST .../accept` or `.../reject`, `GET /v1/ledger/{party_id}` (all in `api.rs:27-39`, listed in `docs/ARCHITECTURE.md` "HTTP surface"). The `Open → Presented` transition needs no requester action; in production the expiry worker ticks every 500 ms (`main.rs:21`, `worker.rs:15-23`), and only the tests deliver `Tick` by hand because they do not start the worker (`common/mod.rs:161-168`). Two soft spots, neither an undocumented call: the Presented body does not state the amount accept will lock (the requester computes it from the selected quote and leg), and the requester has to remember the request id from the 201 because there is no list endpoint.

**(b) Maker journey.** Credit, quote, cancel, observe, and get paid are all documented endpoints. The missing step is **discovery**: there is no endpoint that lists open requests (`api.rs:27-39` has only `GET /v1/requests/{id}`), so a maker must obtain request and leg ids out of band. For a venue described as permissionless this is the one step that requires something outside the API. Everything after that is complete: quote (`POST .../quotes`), cancel while Open (`DELETE /v1/quotes/{id}`), watch state via `GET /v1/requests/{id}`, and payouts or refunds land in `free` without any maker action (`ledger.rs:285-286`, `301-303`).

**(c) Oracle.** `POST /v1/oracle/resolve` accepts all four outcomes and the guard admits both `Locked` and `Disputed` (`engine.rs:454`), so `yes`, `no`, `invalid`, and `disputed` are all reachable from both states in code. Tests cover `Locked → {yes, no, invalid, disputed}` and `Disputed → yes` (`failure_modes.rs:952-977`); `Disputed → {no, invalid, disputed}` have no repo test. The probe confirmed `disputed → disputed` is a 200 no-op and `disputed → invalid` unwinds with full refunds. There is no oracle identity: any caller is the oracle.

---

## Part 2 — `FAILURE_MODES.md` diff

Criterion for VERIFIED: named test exists, passes, and asserts the four things where they apply (HTTP status, request + quote states, numeric balances, conservation). PARTIAL: exists and passes but asserts less than the row or the intro claims. n/a means the item cannot apply to that row (e.g. a tick has no HTTP status).

| Row | Test | Status | Request / quote states | Balances | Conservation | Verdict |
|---|---|---|---|---|---|---|
| H1 | `fm_happy_path_three_legs` `failure_modes.rs:98-155` | `:118` 200, `:141` 200 | `:114`, `:119`, `:142`; quotes `:151-153`; escrows `:120-133` | `:104`, `:106-110`, `:134`, `:136`, `:143-147`, `:149` | `:138`, `:154` | VERIFIED |
| M1 | `fm_leg_unmatched_at_deadline_releases_all` `:161-194` | n/a (tick); GET 200 via `common/mod.rs:358` | `:172`, fail reason `:173-176`, no package `:177-180`, escrows empty `:181`, quotes `:182` | `:167-168`, `:185`, `:187` | `:193`; plus `lock_batch_calls == 0` `:188-192` | VERIFIED |
| M2 | `fm_requester_insufficient_funds_at_accept` `:196-229` | `:206` 402, code `:207` | `:210`, `:211-214`, `:215`, quotes `:216-218` | `:219-223`, `:224-226` | `:228`; `lock_batch_calls == 1` `:227` | VERIFIED |
| M3 | `fm_lock_batch_is_atomic` `:231-271` | n/a (direct ledger call, no HTTP) | n/a (no request); ledger balances before/after `:240`, `:263-267` | `:263-268` | `:270` | VERIFIED (ledger-level by design; row says "two-phase under one mutex", test proves the observable all-or-nothing, mechanism at `ledger.rs:219-276`) |
| A1 | `fm_requester_reject_releases_selected_and_losers` `:277-299` | `:288` 200 | `:285`, `:289-291` | `:292-297` | `:298` (and `assert_balances` conservation `common/mod.rs:324-328`) | VERIFIED |
| A2 | `fm_accept_window_expiry_fails_request` `:301-332` | `:330` 409 (later accept) | `:307`, `:310-314`, `:318-325` | makers `:326-328`; requester not asserted (never held funds) | `:331` | VERIFIED (requester balance omitted; covered by conservation) |
| A3 | `fm_accept_and_tick_race` `:501-549` | `:524` / `:530` | `:521-522`, `:525`, `:528`, `:531-535` | `:526`, `:536-539` | `:543`; both interleavings `:545-548` | VERIFIED. Note the row's clause "accept re-checks `accept_deadline` itself" (`engine.rs:362-379`) is **not** what this test exercises: the mock clock stays inside the window (`:508`, `:511`), so the 409 in the failed branch comes from `expect_state` at `engine.rs:361` after the tick won. The re-check branch has no test (probe passed). |
| A4 | `fm_cancel_and_tick_race` `:551-593` | `:575` / `:580` | `:570-573`, `:577`, `:582` | `:578`, `:583` | `:587` | VERIFIED |
| A5 | `fm_selected_quote_cannot_be_cancelled` `:334-354` | `:342` 409, code `:343` | quote `:344-347`; **request state not asserted** (implied Presented) | `:348-352` | `:353` | PARTIAL (missing request-state assertion; one line) |
| A6 | `fm_quote_expiring_before_accept_window_rejected_at_submit` `:356-395` | `:372`, `:385` 400 with both codes `:373`, `:386` | quotes empty `:388`; request state not asserted | `:389-393` | `:394` | PARTIAL (request state) |
| A7 | `fm_quote_expiring_before_accept_deadline_is_ineligible` `:397-430` | `:421` 200 | `:408`, `:410-412`, `:413-418`, `:422` | `:423-428` | `:429` | VERIFIED |
| A8 | `fm_expired_quote_not_selected_and_released` `:432-451` | n/a (tick) | `:442-444` | `:445-449` | `:450` | VERIFIED |
| A9 | `fm_double_accept_is_409` `:453-471` | `:462` 409, code `:463` | `:464`; quote states not asserted | `:465-469` | `:470` | VERIFIED (quote states implied by prior accept; balance proves no second lock) |
| A10 | `fm_accept_after_terminal_is_409` `:473-499` | `:483-495` three 409s | `:496` | `:497` (unchanged after settle) | `:498` | VERIFIED |
| A11 | `fm_tie_breaks_on_seq` `:763-790` | n/a (tick) | quote states `:788`, selection `:787`; timestamps tie `:783-786`; request state not asserted (package presence implies Presented) | none asserted | `:789` | PARTIAL (no numeric balances; row claims determinism only, so balances are arguably n/a) |
| P1 | `fm_non_owner_cannot_accept_or_reject` `:599-618` | `:608`, `:611` 403, code `:609` | `:613` | `:614-616` | `:617` | VERIFIED |
| P2 | `fm_maker_cannot_cancel_others_quote` `:620-632` | `:627` 403, code `:628` | quote `:629`; request state not asserted | `:630` | `:631` | VERIFIED (request state trivially Open) |
| P3 | `fm_maker_insufficient_funds_at_quote_is_402` `:664-686` | `:681` 402, code `:682` | quotes empty `:683` | `:684` | `:685` | VERIFIED |
| P4 | `fm_quote_too_small_rejected` `:688-707` | `:703` 400, code `:704` | **none** (no quotes-empty or state assertion) | `:705` | `:706` | PARTIAL (no state assertion; balance shows nothing reserved) |
| P5 | `fm_quote_after_presented_is_409` `:709-739` | `:728` 409, code `:729` | quote count `:730-736`; request state not asserted | `:737` | `:738` | VERIFIED (state implied) |
| P6 | `fm_cancel_released_quote_is_409` `:741-761` | `:746-749` 204, `:753` 409, code `:754` | quote state after second cancel not asserted | `:750`, `:755-759` | `:760` | PARTIAL (no quote-state assertion; "no double release" proven by balance) |
| P7 | `fm_missing_party_header_is_401` `:792-813` | `:797` 401, code `:798`; header-present path not 401 `:807-811` | n/a | n/a (no parties funded) | `:812` | VERIFIED |
| P8 | `fm_unknown_ids_are_404` `:815-853` | six 404s `:820-850` | none ("no side effects" shown by balance only) | `:851` | `:852` | VERIFIED (quotes-empty assertion would strengthen) |
| P9 | `fm_invalid_body_rejected` `:855-900` | `:872-876`, `:881-884`, `:886-889`, `:893-896` 400 with codes | none | `:898` | `:899` | VERIFIED (states n/a: nothing was created) |
| P10 | `fm_response_deadline_in_past_rejected` `:902-914` | `:910` 400 ×2, code `:911` | none | none (nothing funded) | `:913` | VERIFIED (nothing to assert) |
| R1 | `fm_resolve_invalid_unwinds_refunds_each_side` `:920-949` | `:927` 200, `:935` 200, `:943-947` 409 | `:936`, `:937` | `:928-932`, `:938-942` | `:948` | VERIFIED |
| R2 | `fm_disputed_then_yes_pays_out` `:951-977` | `:960`, `:970` 200 | `:961`, `:971` | `:962-966`, `:972`, `:973-975` | `:967`, `:976` | VERIFIED |
| R3 | `fm_resolve_before_locked_is_409` `:979-1000` | `:984-988`, `:991` 409, code `:992` | `:993` | `:994-998` | `:999` | VERIFIED |
| X1 | `fm_self_quote_rejected` `:634-662` | ignored; red when forced (`:650`, 201 vs 400) | — | — | — | RED as expected; the only red test |

The intro sentence `docs/FAILURE_MODES.md:3` ("Each test asserts HTTP status, request and quote states, balances, and ledger conservation") is overstated for M1, M3, A8, A11 (no HTTP status by nature), A5, A6, P4, P6 (state assertions missing), and P7, P10 (no balances). Every test does call `assert_conserved` (`failure_modes.rs:138, 154, 193, 228, 270, 298, 331, 353, 394, 429, 450, 470, 498, 543, 587, 617, 631, 661, 685, 706, 738, 760, 789, 812, 852, 899, 913, 948, 967, 976, 999`).

**Reverse diff: tests not named in the table.** `tests/happy_path.rs`: `full_lifecycle_two_legs_settles_yes` (`:14`), `unmatched_leg_fails_request_and_releases_every_reservation` (`:182`), `missing_party_header_is_unauthorized` (`:231`). `tests/settlement.rs`: `settles` with 14 cases (`:26-53`). The table's own scope statement (`docs/FAILURE_MODES.md:3`) is limited to `tests/failure_modes.rs`, so this is consistent, but the two other files are the only coverage of `sell_yes`/`buy_no` legs and of exact wire-body fixtures.

**Reverse diff: error paths.**

| Site | Variant / status | Test |
|---|---|---|
| `engine.rs:267` | `DeadlineInPast` 400 | P10 `:903-914` |
| `engine.rs:270` → `request.rs:156-158` | `EmptyLegs` 400 | P9 `:885-889` |
| `engine.rs:286-289`, `:291`, `:324-327`, `:332`, `:354-357`, `:427-430`, `:450-453`, `:236-243` | `NotFound` 404 | P8 `:816-853` (request, leg, quote, resolve, GET) |
| `engine.rs:290` | `WrongState` 409 (quote after Presented) | P5 `:710-739` |
| `engine.rs:292` | `QuoteExpired` 400 | A6 `:385-386` |
| `engine.rs:295` | `QuoteTooSmall` 400 | P4 `:689-707` |
| `engine.rs:298` | `QuoteExpiresBeforeAcceptWindow` 400 | A6 `:372-373` |
| `engine.rs:313` | `InsufficientFunds` 402 (reserve) | P3 `:665-686` |
| `engine.rs:333`, `:358`, `:431` | `NotOwner` 403 | P2 `:621-632`, P1 `:600-618` |
| `engine.rs:336` | `WrongState` 409 (cancel, request not Open) | A5 `:335-354` |
| `engine.rs:337` | `QuoteNotLive` 409 | P6 `:742-761` |
| `engine.rs:361`, `:434` | `WrongState` 409 (accept/reject not Presented) | A9, A10, A2 `:329-330` |
| `engine.rs:362-379` | accept-side deadline re-check → 409 + `Failed` + releases | **none** (probe only) |
| `engine.rs:384-392` | `InsufficientFunds` 402 (lock_batch) | M2 `:197-229` |
| `engine.rs:393-395` | `UnknownReservation` → `unreachable!` | ledger side unit `ledger.rs:451`; engine side unreachable by construction |
| `engine.rs:454-459` | `WrongState` 409 (resolve) | R3, A10, R1 `:943-947` |
| `engine.rs:701-702`, `:817` → `api.rs:107` | `Unavailable` 503 | unit mapping only `api.rs:365-368`; **no integration test** (requires a dead actor) |
| `api.rs:64-71` | 401 missing / non-UUID header | P7 covers missing `:793-798`; **non-UUID header untested** |
| `api.rs:232-234` | `ZeroAmount` 400 | unit only `api.rs:397-403`; **no HTTP test** |
| `api.rs:285-287` | `ZeroSize` 400 | unit only `api.rs:397-403`; **no HTTP test** |
| `api.rs:288` | `InvalidPrice` 400 | P9 `:861-877` |
| `api.rs:180` | `InvalidContractId` 400 | P9 `:890-896` |
| `api.rs:181` | `InvalidContractDescription` 400 | unit only `api.rs:444-447`; **no HTTP test** |
| `api.rs:182-187` | `ZeroNotional` 400 | P9 `:878-884` |

---

## Part 3 — Claim-by-claim audit

### `ASSUMPTIONS.md`

| Line | Claim | Verdict | Evidence |
|---|---|---|---|
| 5 | identity is the header, trusted; authorization is ownership only | VERIFIED | extractor `api.rs:45-74` parses a UUID and nothing else; ownership checks `engine.rs:333-335`, `358-360`, `431-433`; no other authz anywhere in `api.rs:27-39` |
| 6 | resolve is ungated; any caller can settle a Locked request | VERIFIED | handler `api.rs:332-341` takes `State` and `Json` only, no `Party`; engine `engine.rs:445-491` has no caller parameter. **Cheapest exploit:** the requester of a Locked `buy_yes` leg posts `{"request_id": <own>, "outcome": "yes"}` and receives `n` per leg (`engine.rs:467-473`); one unauthenticated `curl`. |
| 7 | one outcome resolves every leg | VERIFIED | `engine.rs:462`, `478` iterate `req.escrows`; `ResolveBody` has no `leg_id` `api.rs:209-213`; mixed-side multi-leg requests settle with one call in `settlement.rs:39-48` |
| 8 | Yes/No pay full notional; Invalid refunds each poster; Disputed holds | VERIFIED | `engine.rs:461-475`, `477-486`, `488`; chunks sum to `n` `request.rs:118-119` + `money.rs:120-122`; tests `settlement.rs:150-160`, R1 `:938-942`, R2 `:962-966` |
| 9 | Locked and Disputed have no timer | VERIFIED | `engine.rs:524-528` empty arms; `EngineConfig` has only `accept_window` `engine.rs:27-30` |
| 10 | requester posts nothing at Open; reject is free | VERIFIED | `submit_request` `engine.rs:260-273` makes no ledger call; requester funds first touched via `FromFree` `engine.rs:572-575`; `reject` `engine.rs:422-442` charges nothing; H1 `:106-110`, A1 `:292-297` |
| 11 | maker collateral = escrow side at quote price for full notional; extra size reserves nothing | VERIFIED | `engine.rs:313` + `request.rs:70-80` use `leg.notional`, never `size`; `happy_path.rs:69-72` (size 2_500, notional 2_000, reserved 1_300 = 6_500 × 2_000 / 10_000) |
| 12 | firm from submit; cancel only Open + Live; Selected cannot be pulled | VERIFIED | `engine.rs:336-339`; A5 `:335-354`, P6 `:742-761` |
| 13 | losers reserved through Presented, released at accept, reject, or window expiry | VERIFIED, with an undocumented nuance | Release sites for a non-selected quote after Presented: (1) accept success `engine.rs:594-598`; (1b) accept refused for funds `385-390` → `666`; (2) reject `435-440` → `666`; (3) window expiry via tick `521` → `666` and via late accept `369-374` → `666`. No fourth site: `release_quotes` callers are `340` (cancel, requires Open), `502` (expiry, only in the `Open` arm), and `666`. **Nuance:** a loser's own `expires_at` is not honoured while Presented (`515-523` checks only `accept_deadline`) and it cannot be cancelled (`336`), so its collateral can be held past its stated expiry for up to `accept_window` (probe: stayed `live`, reserved 500, cancel 409). Not stated in the doc. |
| 14 | atomic fill: `size >= notional`, every leg must match | VERIFIED | `engine.rs:295-297`, `matching.rs:25`, `present_or_fail` `engine.rs:615-627`; M1 `:162-194`, P4 |
| 15 | `u64` minor units; buyer floored, seller remainder; overflow panics rather than wraps | VERIFIED | `Amount` `Add`/`Sub` are `checked_*().expect()` `money.rs:41-55` and `AddAssign`/`SubAssign` route through them `57-67`, so they panic in every profile regardless of `overflow-checks`; `Cargo.toml:1-18` sets no profile flags (so plain integer ops *would* wrap in release, but none touch money: the only plain arithmetic is the `u128` product `money.rs:113-114`, bounded by `u64::MAX × 9_999`, and the handle counter `ledger.rs:115`); `Seq::next` is checked `ids.rs:118-120`; tests `money.rs:149`, `185`, `198` |
| 16 | prices in bps `1..=9_999`; No is Yes at `1 − p` | VERIFIED | `money.rs:98-103`; `state.rs:23-25`; `request.rs:75-79`, `109-113`; P9 `:861-877`; unit `request.rs:256`, `292` |
| 17 | deadlines client-supplied, checked only against venue clock; no maximum horizon | VERIFIED as stated, **consequence understated** | `engine.rs:266-269` is the only check. Because there is no cap, a `response_deadline` within `accept_window` of chrono's maximum makes `req.response_deadline + self.config.accept_window` at `engine.rs:298` panic (`DateTime + TimeDelta overflowed`), which kills the actor task (`engine.rs:687-691`) and turns every later request into 503 (`engine.rs:701`, `api.rs:107`). Reproduced by probe. The consequence is not "clock skew is the client's problem" but "one request halts the venue". See Part 4 #1. |
| 18 | `Tick` carries `now`; window starts at the presenting tick; matching requires outliving `now + accept_window` | VERIFIED | `engine.rs:146`, `505-512` (`now + accept_window`), `matching.rs:27`; A7 `:397-430` |
| 19 | boundaries | VERIFIED | accept at exactly `accept_deadline` allowed: `engine.rs:362` uses `>` (tick side `516` also `>`; test `:309-314` covers the tick, probe covered the accept). `expires_at == now` is expired: `engine.rs:292` `<=`, `engine.rs:503` `<=`, `matching.rs:26` `>`; A6 `:375-386`, unit `matching.rs:117-129`. Deadline instant presents: `engine.rs:505` `>=`; every `advance_to(RESPONSE_DEADLINE_SECS)` (e.g. `:112-114`). Quote expiring exactly at `accept_deadline` is eligible: `matching.rs:27` `>=`, unit `matching.rs:136-148`. |
| 20 | ties on submit order, never timestamps | VERIFIED | `matching.rs:31`, `33` key `(price, seq)`; seq stamped `engine.rs:310`, `315`; A11 `:763-790`, unit `matching.rs:207` |
| 21 | one actor, bounded queue, serial mutations, back-pressure not 503 | VERIFIED | `engine.rs:685-693` channel of 256; `send().await` blocks when full `engine.rs:698-701`, `814-817`; `Unavailable` only when the receiver is gone; races A3, A4 |
| 22 | "Ledger, oracle, and clock are in-memory; nothing persists" | VERIFIED for ledger and clock, **wording wrong for oracle** | `MockLedger` `ledger.rs:130-134`, `main.rs:16`; `SystemClock` `main.rs:14`; requests live in `HashMap`s `engine.rs:161-167`. There is no oracle component at all; resolution is an HTTP caller (`api.rs:332-341`). |
| 23 | requester may quote its own request (X1) | VERIFIED | no requester/maker comparison in `engine.rs:276-320`; `:634-662` ignored and red when forced |

### `docs/RESOLUTION.md`

| Line | Claim | Verdict | Evidence |
|---|---|---|---|
| 7 | reservations gone by Locked; two handles per leg; sum to `n` | VERIFIED | `engine.rs:590-598` (Selected consumed by batch, Live released); handles per item `ledger.rs:273-275`, two items per leg `engine.rs:571-575`; sum `request.rs:118-119` |
| 11 | must be Locked or Disputed, else 409 with no ledger call | VERIFIED | `engine.rs:454-459` returns before any ledger call; R3 `:994-998` shows balances untouched |
| 15 | `yes` → both chunks to Yes-buyer → Settled, terminal | VERIFIED | `engine.rs:467-468`, `472-473`, `475`; terminal via guard `454`; H1, `settlement.rs:28, 30` |
| 16 | `no` → both chunks to Yes-seller → Settled | VERIFIED | `engine.rs:469-470`, `472-473`; `settlement.rs:29, 31` |
| 17 | `invalid` → refund each chunk to poster → Unwound | VERIFIED | `engine.rs:477-486`; `ledger.rs:296-304`; R1 `:938-942` |
| 18 | `disputed` → nothing → Disputed, not terminal | VERIFIED | `engine.rs:488`; R2 `:959-967` then `:969-976` |
| 20 | handles consumed; second `Resolve` is 409 | VERIFIED | ledger removes on payout/refund `ledger.rs:282`, `298` (repeat is a no-op, unit `ledger.rs:509`, `371`); engine removes its map entry `engine.rs:463-466`, `479-482`; second resolve blocked by `454` (A10 `:491-495`, R1 `:943-947`) |
| 24 | repeated `disputed` is a 200 no-op | VERIFIED by probe, **no repo test** | `engine.rs:454` admits Disputed; `488` re-sets it; no ledger call |
| 24 | from Disputed, `yes`/`no` pay out exactly as from Locked; `invalid` unwinds | VERIFIED (`yes` by R2; `invalid` by probe; `no` untested) | escrow map is untouched by the Disputed arm, so `463-466`/`479-482` find the handles |
| 28 | `Tick` skips Locked and Disputed | VERIFIED | `engine.rs:524-528` |
| 30-35 | timeout policy genuinely absent | VERIFIED | no `resolution_timeout`/`unwind_timeout` anywhere in `src/` (config `engine.rs:27-30`); no locked-at timestamp on `RfqRequest` `request.rs:127-146` |
| 39 | engine never reads contract text | VERIFIED | `ContractId`/`ContractDescription` are opaque newtypes `ids.rs:44-64`, `67-102`; no other reference in `engine.rs` or `matching.rs` |
| 43-46 | invariants | VERIFIED | escrow created only in `accept` `engine.rs:382`, destroyed only in `resolve` `472-473`, `483-484`; venue identity asserted by `assert_conserved` `common/mod.rs:401-417` in every failure-mode test |

### Conservation "asserted after every scenario"

`docs/ARCHITECTURE.md:128` says "The tests assert both after every scenario" (per-party conservation and the venue-escrow identity). Reality: `assert_conserved` (`common/mod.rs:385-418`, both checks) is called in every test of `tests/failure_modes.rs` (lines listed under Part 2). `tests/happy_path.rs` and `tests/settlement.rs` never call it; they call `assert_balances` (`common/mod.rs:320-328`), which checks only per-party `conservation_holds` (`ledger.rs:146-150`) and not the venue-escrow identity. `missing_party_header_is_unauthorized` (`happy_path.rs:231-241`) checks neither (nothing is funded). So: PARTIAL, doc overstated for 4 of 32 integration tests.

---

## Part 4 — Findings

Ordered by money impact, then doc fixes, code gaps, coverage holes. "Pre-sub" = fixable in ≤ 15 minutes before submission; "gap" = known-gap candidate.

| # | Finding | Where | Class |
|---|---|---|---|
| 1 | **A far-future `response_deadline` halts the venue.** Opening a request with a deadline within `accept_window` of chrono `MAX_UTC` succeeds (201); the first quote on it evaluates `response_deadline + accept_window` and panics (`DateTime + TimeDelta overflowed`), the actor task exits, and every endpoint returns 503 thereafter. All balances become unreachable and are lost on restart. Reproduced by probe. Fix: `checked_add_signed` at `src/engine.rs:298` (and defensively `:511`) returning `DeadlineInPast`/400, or a maximum horizon at `:267`. | `src/engine.rs:298`, `:511`, `:267` | **Pre-sub, money impact: venue-wide freeze** |
| 2 | Resolve is ungated: any party can settle any Locked/Disputed request in its own favour with one call. Documented (`ASSUMPTIONS.md:6`); out of scope by instruction. | `src/api.rs:332-341` | gap (documented) |
| 3 | The accept-side deadline re-check, which fails the request and releases every maker, has no test. `fm_accept_and_tick_race` never has the clock past the deadline at accept time. Probe: behaves correctly. One test: present, `set(at(ACCEPT_DEADLINE_SECS + 1))` without ticking, accept → 409, `failed(accept_window_expired)`, makers `bal(SIDE_LOCK,0,0)`, conserved. Add the exact-deadline success case in the same test to pin `ASSUMPTIONS.md:19`. | `tests/failure_modes.rs` (new test); code at `src/engine.rs:362-379` | Pre-sub, coverage on a money path |
| 4 | A losing quote past its own `expires_at` stays reserved and uncancellable while the request is Presented, for up to `accept_window`. Not a bug against the stated rule, but undocumented. Doc fix: one clause on `ASSUMPTIONS.md:13` ("a quote's own expiry is not honoured once Presented"). Code alternative (behaviour change): release expired non-selected `Live` quotes in the Presented tick arm. | `ASSUMPTIONS.md:13`; code `src/engine.rs:515-523`, `:336` | Pre-sub (doc) / gap (code) |
| 5 | Makers cannot discover requests: no list endpoint. A permissionless venue whose makers need out-of-band ids. Doc fix: state it in `ASSUMPTIONS.md` and `docs/ARCHITECTURE.md` "HTTP surface". Adding `GET /v1/requests` is a feature. | `src/api.rs:27-39`; docs | Pre-sub (doc) / gap (code) |
| 6 | Presented response does not state what accept will lock; the requester must compute `floor(p·n)` or `n − floor(p·n)` per leg from `quotes[].price_bps` and `legs[].side/notional`. Doc fix: say so in `docs/ARCHITECTURE.md` under Presented. | `src/domain/request.rs:126-146`, `src/engine.rs:633` | Pre-sub (doc) / gap (field) |
| 7 | `Disputed → {invalid, no, disputed}` untested although `docs/RESOLUTION.md:24` asserts all three. Probe confirmed `disputed` no-op and `invalid` unwind. One test extending `fm_disputed_then_yes_pays_out`'s shape covers all three. | `tests/failure_modes.rs`; code `src/engine.rs:454`, `:477-488` | Pre-sub, coverage on a money path |
| 8 | `docs/ARCHITECTURE.md:128` overstates conservation: happy-path and settlement tests check per-party conservation but not the venue-escrow identity. Either soften the sentence or add `v.assert_conserved().await` at the end of `happy_path.rs:180`, `:229` and `settlement.rs:160`. | `docs/ARCHITECTURE.md:128`; tests | Pre-sub |
| 9 | `docs/FAILURE_MODES.md:3` overstates per-test assertions (see Part 2: M1, M3, A8, A11 no status; A5, A6, P4, P6 no state; P7, P10 no balances). Change to "where applicable", or add the one-line assertions in A5 (`request["state"] == "presented"`), A6, P4 (`quotes == []`), P6 (`released`). | `docs/FAILURE_MODES.md:3`; `tests/failure_modes.rs:344`, `:388`, `:705`, `:755` | Pre-sub |
| 10 | `ASSUMPTIONS.md:22` names an in-memory "oracle"; no oracle component exists. Reword to "ledger and clock are in-memory; the oracle is whoever calls resolve". | `ASSUMPTIONS.md:22` | Pre-sub |
| 11 | `docs/FAILURE_MODES.md:13` (A3) says "accept re-checks `accept_deadline` itself" as the mechanism for the row, but the named test does not exercise that branch (Part 2). Fold into #3 or reword the row to "actor serialises; the late command sees Locked or Failed". | `docs/FAILURE_MODES.md:13` | Pre-sub |
| 12 | Self-quote allowed (X1). With #2 this lets a requester wash-trade against itself at any price, but no third party loses money; documented and ignored. | `src/engine.rs:276-320` | gap (documented) |
| 13 | HTTP-level coverage holes with no money impact: 503 `engine_unavailable`; non-UUID `x-party-id`; `invalid_contract_description`; `zero_amount`; `zero_size` (all unit-only). | `src/api.rs:68-71`, `:107`, `:181`, `:232`, `:285` | gap |
| 14 | `tests/failure_modes.rs` is `buy_yes`-only; failure and race paths never run with the maker as Yes-buyer. Side-dependent code is covered by `settlement.rs` and `happy_path.rs`, and release logic is side-agnostic, so low value. | `tests/common/mod.rs:459-462` | gap |

### Three questions a hostile reviewer opens with

1. **"I can take your venue down with one request. What happens to everyone's money?"** True today: a `response_deadline` within 60 seconds of chrono's maximum passes validation, the first quote overflows at `src/engine.rs:298`, the actor dies, and every endpoint returns 503 until a restart that loses all in-memory state. The fix is a checked add returning 400 or a deadline cap; neither is in the code yet.
2. **"Anyone can call resolve. Why would a maker post collateral here?"** They should not, beyond this exercise. Gating the oracle was ruled out of scope with auth; the engine's guarantees are that capital is conserved, never half-locked, and returned to its poster on Invalid under any outcome, not that the outcome is authentic.
3. **"Row A3 says accept re-checks the deadline itself. Which test proves that branch?"** None in the repo. The race test keeps the clock inside the window, so its 409 comes from the state check after the tick wins. The branch at `src/engine.rs:362-379` was exercised only by a probe in this review, where it behaved correctly; it needs a test before submission.
