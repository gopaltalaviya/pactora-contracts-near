//! Unit tests on the mocked NEAR runtime. Each rule here mirrors a v3 EVM
//! test; the property test at the end checks the v3 invariants after every
//! step of random sequences.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, Gas, NearToken, PromiseResult};

use crate::types::*;
use crate::Contract;

const TOKEN: &str = "usdc.test";
const CONTRACT: &str = "pactora.test";
const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const T0: u64 = 1_800_000_000; // seconds

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn params() -> Params {
    Params {
        protocol_fee_bps: 100,
        min_fee: U128(100),
        min_job_amount: U128(10_000),
        max_job_amount: U128(10_000_000),
        fast_path_max: U128(30_000),
        review_window: 600,
        max_accept_window: 3_000,
        max_work_window: 18_000,
        dispute_timeout: 3_600,
        bond_bps: 2_000,
        min_bond: U128(10_000),
        dispute_bond_bps: 1_000,
        min_dispute_bond: U128(10_000),
        min_stake: U128(10_000_000),
        withdraw_delay: 604_800,
        default_per_task_cap: U128(5_000_000),
        default_daily_cap: U128(20_000_000),
    }
}

/// Test harness: one contract, a clock, and a model of the token balance the
/// contract should hold (deposits in, successful transfers out).
struct H {
    c: Contract,
    now: u64,
    held: u128,
}

fn ctx(pred: &str, deposit: u128, now: u64) {
    let mut b = VMContextBuilder::new();
    b.current_account_id(acc(CONTRACT))
        .predecessor_account_id(acc(pred))
        .signer_account_id(acc(pred))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .account_balance(NearToken::from_yoctonear(1_000 * NEAR))
        .block_timestamp(now * 1_000_000_000)
        .prepaid_gas(Gas::from_tgas(300));
    testing_env!(b.build());
}

impl H {
    fn new() -> Self {
        // The mocked runtime keeps storage across `testing_env!` calls; a new
        // harness starts from empty storage, like a fresh deployment.
        ctx("owner.test", 0, T0);
        near_sdk::mock::with_mocked_blockchain(|b| {
            let _ = b.take_storage();
        });
        let c = Contract::new(
            acc("owner.test"),
            acc(TOKEN),
            acc("dao.test"),
            acc("pauser.test"),
            acc("fees.test"),
            params(),
        );
        H {
            c,
            now: T0,
            held: 0,
        }
    }

    fn as_(&self, who: &str) {
        ctx(who, 0, self.now);
    }

    fn as_yocto(&self, who: &str) {
        ctx(who, 1, self.now);
    }

    fn advance(&mut self, s: u64) {
        self.now += s;
    }

    /// Register an agent owned by `owner` with wallet `wallet`, accepted.
    fn agent(&mut self, owner: &str, wallet: &str) -> AgentId {
        ctx(owner, NEAR, self.now);
        let id = self
            .c
            .register_agent(acc(wallet), "ipfs://card".into(), None, None);
        self.as_(wallet);
        self.c.accept_wallet(id);
        id
    }

    fn fund(&mut self, id: AgentId, amount: u128) {
        self.as_(TOKEN);
        let _ = self.c.ft_on_transfer(
            acc("funder.test"),
            U128(amount),
            format!(r#"{{"Fund":{{"agent_id":{id}}}}}"#),
        );
        self.held += amount;
    }

    fn stake(&mut self, id: AgentId, amount: u128) {
        self.as_(TOKEN);
        let _ = self.c.ft_on_transfer(
            acc("funder.test"),
            U128(amount),
            format!(r#"{{"Stake":{{"agent_id":{id}}}}}"#),
        );
        self.held += amount;
    }

    /// Run the callback of the oldest outgoing transfer.
    fn finish_send(&mut self, ok: bool, to: &str, amount: u128, refund: Refund) {
        let mut b = VMContextBuilder::new();
        b.current_account_id(acc(CONTRACT))
            .predecessor_account_id(acc(CONTRACT))
            .block_timestamp(self.now * 1_000_000_000)
            .prepaid_gas(Gas::from_tgas(300));
        let result = if ok {
            PromiseResult::Successful(vec![])
        } else {
            PromiseResult::Failed
        };
        testing_env!(
            b.build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![result]
        );
        assert_eq!(self.c.on_send(acc(to), U128(amount), refund), ok);
        if ok {
            self.held -= amount;
        }
    }

    fn balance(&self, id: AgentId) -> u128 {
        self.c.purse_of(id).unwrap().balance.0
    }

    fn job(&self, id: JobId) -> Job {
        self.c.get_job(id).unwrap()
    }

    /// I1: every token held is in exactly one bucket.
    fn check_i1(&self) {
        let t = self.c.get_totals();
        assert_eq!(
            t.sum.0, self.held,
            "I1: totals {:?} vs held {}",
            t, self.held
        );
    }
}

/// A client (alice) with funds and a staked worker (bob), both hireable.
fn market() -> (H, AgentId, AgentId) {
    let mut h = H::new();
    let client = h.agent("alice.test", "c.alice.test");
    let worker = h.agent("bob.test", "w.bob.test");
    h.fund(client, 10_000_000);
    h.stake(worker, 20_000_000);
    (h, client, worker)
}

const SPEC: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const RESULT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn panics<F: FnOnce()>(f: F) -> String {
    let r = catch_unwind(AssertUnwindSafe(f));
    match r {
        Ok(()) => panic!("expected a panic"),
        Err(e) => e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default(),
    }
}

fn expect_panic<F: FnOnce()>(msg: &str, f: F) {
    let got = panics(f);
    assert!(
        got.contains(msg),
        "expected panic containing {msg:?}, got {got:?}"
    );
}

fn create(h: &mut H, client: AgentId, worker: AgentId, amount: u128) -> JobId {
    h.as_("c.alice.test");
    h.c.create_job(client, worker, U128(amount), SPEC.into(), 300, 1_800)
}

// ─────────────────────────── registry ───────────────────────────

#[test]
fn registration_needs_the_wallet_to_accept() {
    let mut h = H::new();
    ctx("alice.test", NEAR, h.now);
    let id =
        h.c.register_agent(acc("c.alice.test"), "uri".into(), None, None);
    let a = h.c.get_agent(id).unwrap();
    assert_eq!(a.wallet, None);
    assert_eq!(a.pending_wallet, Some(acc("c.alice.test")));
    h.as_("mallory.test");
    expect_panic("NotPendingWallet", || h.c.accept_wallet(id));
    h.as_("c.alice.test");
    h.c.accept_wallet(id);
    assert_eq!(h.c.agent_of_wallet(acc("c.alice.test")), Some(id));
}

#[test]
fn registration_storage_is_paid_by_the_owner() {
    let mut h = H::new();
    ctx("alice.test", 0, h.now);
    expect_panic("InsufficientStorage", || {
        h.c.register_agent(acc("c.alice.test"), "uri".into(), None, None);
    });
    ctx("alice.test", NEAR, h.now);
    let id =
        h.c.register_agent(acc("c.alice.test"), "uri".into(), None, None);
    let left = h.c.storage_balance_of(id).0;
    assert!(
        left > 0 && left < NEAR,
        "the registration's own storage was billed"
    );
}

#[test]
fn one_wallet_serves_one_agent() {
    let mut h = H::new();
    let a = h.agent("alice.test", "w.test");
    ctx("bob.test", NEAR, h.now);
    let b = h.c.register_agent(acc("w.test"), "uri".into(), None, None);
    h.as_("w.test");
    expect_panic("WalletTaken", || h.c.accept_wallet(b));
    assert_eq!(h.c.agent_of_wallet(acc("w.test")), Some(a));
}

#[test]
fn wallet_rotation_keeps_the_old_key_until_the_new_one_accepts() {
    let (mut h, client, worker) = market();
    h.as_yocto("alice.test");
    h.c.propose_wallet(client, acc("c2.alice.test"));
    let job = create(&mut h, client, worker, 100_000);
    h.as_("c2.alice.test");
    h.c.accept_wallet(client);
    assert_eq!(h.c.agent_of_wallet(acc("c.alice.test")), None);
    h.as_("c.alice.test");
    expect_panic("NotAgentWallet", || h.c.cancel(job));
    h.as_("c2.alice.test");
    h.c.cancel(job);
}

#[test]
fn only_the_owner_changes_an_agent() {
    let (mut h, client, _) = market();
    h.as_yocto("c.alice.test");
    expect_panic("NotAgentOwner", || h.c.set_caps(client, U128(1), U128(2)));
    h.as_("alice.test");
    expect_panic("Requires attached deposit of exactly 1 yoctoNEAR", || {
        h.c.set_caps(client, U128(1), U128(2))
    });
}

// ─────────────────────────── hire checks, v3 order ───────────────────────────

#[test]
fn hire_checks_refuse_in_v3_order() {
    let (mut h, client, worker) = market();
    h.as_("c.alice.test");
    expect_panic("ZeroAmount", || {
        h.c.create_job(client, worker, U128(0), SPEC.into(), 300, 1_800);
    });
    expect_panic("AmountBelowMinimum", || {
        h.c.create_job(client, worker, U128(9_999), SPEC.into(), 300, 1_800);
    });
    expect_panic("SelfDealing", || {
        h.c.create_job(client, client, U128(100_000), SPEC.into(), 300, 1_800);
    });
    h.as_("w.bob.test");
    expect_panic("NotAgentWallet", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 1_800);
    });
}

#[test]
fn same_owner_cannot_hire_itself_through_two_agents() {
    let mut h = H::new();
    let a = h.agent("alice.test", "a1.alice.test");
    let b = h.agent("alice.test", "a2.alice.test");
    h.fund(a, 1_000_000);
    h.stake(b, 20_000_000);
    h.as_("a1.alice.test");
    expect_panic("SameOwner", || {
        h.c.create_job(a, b, U128(100_000), SPEC.into(), 300, 1_800);
    });
}

#[test]
fn a_worker_without_stake_or_wallet_or_frozen_is_not_hireable() {
    let (mut h, client, worker) = market();
    // No stake.
    let lazy = h.agent("carol.test", "w.carol.test");
    h.as_("c.alice.test");
    expect_panic("NotHireable", || {
        h.c.create_job(client, lazy, U128(100_000), SPEC.into(), 300, 1_800);
    });
    // Registered but the wallet never accepted.
    ctx("dave.test", NEAR, h.now);
    let unaccepted =
        h.c.register_agent(acc("w.dave.test"), "uri".into(), None, None);
    h.stake(unaccepted, 20_000_000);
    h.as_("c.alice.test");
    expect_panic("NotHireable", || {
        h.c.create_job(client, unaccepted, U128(100_000), SPEC.into(), 300, 1_800);
    });
    // Frozen by its owner.
    h.as_("bob.test");
    h.c.freeze_agent(worker);
    assert!(!h.c.is_hireable(worker));
    h.as_("c.alice.test");
    expect_panic("NotHireable", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 1_800);
    });
}

#[test]
fn amount_and_window_limits() {
    let (mut h, client, worker) = market();
    h.as_("c.alice.test");
    expect_panic("AmountAboveMaximum", || {
        h.c.create_job(client, worker, U128(10_000_001), SPEC.into(), 300, 1_800);
    });
    expect_panic("WindowTooLong", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 0, 1_800);
    });
    expect_panic("WindowTooLong", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 18_001);
    });
    expect_panic("BadHash", || {
        h.c.create_job(client, worker, U128(100_000), "abc".into(), 300, 1_800);
    });
}

// ─────────────────────────── caps ───────────────────────────

#[test]
fn caps_bound_what_a_stolen_agent_key_can_spend() {
    let (mut h, client, worker) = market();
    h.as_yocto("alice.test");
    h.c.set_caps(client, U128(200_000), U128(300_000));
    h.as_("c.alice.test");
    expect_panic("PerTaskCapExceeded", || {
        h.c.create_job(client, worker, U128(200_001), SPEC.into(), 300, 1_800);
    });
    h.c.create_job(client, worker, U128(200_000), SPEC.into(), 300, 1_800);
    expect_panic("DailyCapExceeded", || {
        h.c.create_job(client, worker, U128(100_001), SPEC.into(), 300, 1_800);
    });
    // And it can never take money out: withdraw needs the owner's full key.
    h.as_yocto("c.alice.test");
    expect_panic("NotAgentOwner", || {
        let _ = h.c.withdraw(client, U128(1), None);
    });
    // 25 hours later the window has rolled.
    h.advance(25 * 3_600);
    h.as_("c.alice.test");
    h.c.create_job(client, worker, U128(200_000), SPEC.into(), 300, 1_800);
}

#[test]
fn spending_more_than_the_balance_is_refused() {
    let mut h = H::new();
    let client = h.agent("alice.test", "c.alice.test");
    let worker = h.agent("bob.test", "w.bob.test");
    h.fund(client, 50_000);
    h.stake(worker, 20_000_000);
    h.as_("c.alice.test");
    expect_panic("InsufficientBalance", || {
        h.c.create_job(client, worker, U128(50_001), SPEC.into(), 300, 1_800);
    });
}

// ─────────────────────────── escrow paths ───────────────────────────

#[test]
fn happy_path_pays_the_worker_and_rates_both() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 1_000_000);
    assert_eq!(h.balance(client), 9_000_000);
    h.check_i1();
    h.as_("w.bob.test");
    h.c.accept_job(id);
    assert_eq!(h.c.stake_of(worker).unwrap().locked.0, 200_000); // 20% bond
    h.c.submit_result(id, RESULT.into());
    h.as_("c.alice.test");
    h.c.approve(id, Some(SPEC.into()));
    let j = h.job(id);
    assert_eq!(j.state, JobState::Settled);
    assert_eq!(j.fee.0, 10_000); // 1%
    assert_eq!(h.balance(worker), 990_000);
    assert_eq!(h.c.stake_of(worker).unwrap().locked.0, 0);
    assert_eq!(h.c.worker_record(worker).unwrap().completed, 1);
    assert_eq!(h.c.client_record(client).unwrap().paid, 1);
    assert_eq!(h.c.verdict_of(id), Some(100));
    h.check_i1();
}

#[test]
fn approve_still_works_after_the_review_window() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.c.submit_result(id, RESULT.into());
    h.advance(10_000);
    h.as_("c.alice.test");
    h.c.approve(id, None);
    assert_eq!(h.job(id).state, JobState::Settled);
}

#[test]
fn cancel_works_even_after_the_accept_deadline() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.advance(10_000);
    h.as_("c.alice.test");
    h.c.cancel(id);
    assert_eq!(h.job(id).state, JobState::Refunded);
    assert_eq!(h.balance(client), 10_000_000);
    assert_eq!(h.c.verdict_of(id), None);
    h.check_i1();
}

#[test]
fn expire_unaccepted_refunds_after_the_deadline_only() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.as_("anyone.test");
    expect_panic("DeadlineNotPassed", || h.c.expire_unaccepted(id));
    h.advance(301);
    h.as_("anyone.test");
    h.c.expire_unaccepted(id);
    assert_eq!(h.balance(client), 10_000_000);
    h.check_i1();
}

#[test]
fn undelivered_work_costs_the_worker_its_bond() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 1_000_000);
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.advance(300 + 1_800 + 1);
    h.as_("w.bob.test");
    expect_panic("DeadlinePassed", || h.c.submit_result(id, RESULT.into()));
    h.as_("anyone.test");
    h.c.expire_undelivered(id);
    assert_eq!(h.job(id).outcome, Some(Outcome::Failed));
    // Refund plus the 200_000 bond.
    assert_eq!(h.balance(client), 10_000_000 + 200_000);
    let s = h.c.stake_of(worker).unwrap();
    assert_eq!((s.available.0, s.locked.0), (19_800_000, 0));
    assert_eq!(h.c.worker_record(worker).unwrap().failed, 1);
    h.check_i1();
}

#[test]
fn a_silent_client_is_auto_approved_and_rated_50() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.c.submit_result(id, RESULT.into());
    h.as_("anyone.test");
    expect_panic("DeadlineNotPassed", || h.c.auto_approve(id));
    h.advance(601);
    h.as_("anyone.test");
    h.c.auto_approve(id);
    let r = h.c.client_record(client).unwrap();
    assert_eq!((r.silent, r.reviews, r.review_score_sum), (1, 1, 50));
    h.check_i1();
}

fn disputed(h: &mut H, client: AgentId, worker: AgentId) -> JobId {
    let id = create(h, client, worker, 1_000_000);
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.c.submit_result(id, RESULT.into());
    h.as_("c.alice.test");
    h.c.dispute(id, Some(SPEC.into()));
    id
}

#[test]
fn dispute_takes_the_bond_from_the_client_balance() {
    let (mut h, client, worker) = market();
    let id = disputed(&mut h, client, worker);
    assert_eq!(h.balance(client), 10_000_000 - 1_000_000 - 100_000); // 10% bond
    assert_eq!(h.job(id).state, JobState::Disputed);
    assert_eq!(h.c.get_totals().dispute_bonds.0, 100_000);
    h.check_i1();
}

#[test]
fn dispute_after_the_review_window_is_refused() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.c.submit_result(id, RESULT.into());
    h.advance(601);
    h.as_("c.alice.test");
    expect_panic("DeadlinePassed", || h.c.dispute(id, None));
}

#[test]
fn only_the_arbiter_rules() {
    let (mut h, client, worker) = market();
    let id = disputed(&mut h, client, worker);
    h.as_("owner.test");
    expect_panic("NotArbiter", || h.c.resolve_dispute(id, true));
}

#[test]
fn ruling_for_the_worker_pays_it_the_job_and_the_bond() {
    let (mut h, client, worker) = market();
    let id = disputed(&mut h, client, worker);
    h.as_("dao.test");
    h.c.resolve_dispute(id, true);
    assert_eq!(h.balance(worker), 990_000 + 100_000);
    assert_eq!(h.c.client_record(client).unwrap().disputes_rejected, 1);
    assert_eq!(
        h.c.verdict_of(id),
        None,
        "a disputed settlement is not attributable"
    );
    h.check_i1();
}

#[test]
fn ruling_for_the_client_refunds_it_and_releases_the_worker_bond() {
    let (mut h, client, worker) = market();
    let id = disputed(&mut h, client, worker);
    h.as_("dao.test");
    h.c.resolve_dispute(id, false);
    assert_eq!(h.balance(client), 10_000_000);
    let s = h.c.stake_of(worker).unwrap();
    assert_eq!(
        (s.available.0, s.locked.0),
        (20_000_000, 0),
        "released, not seized"
    );
    assert_eq!(h.c.worker_record(worker).unwrap().disputes_lost, 1);
    assert_eq!(h.c.verdict_of(id), Some(0));
    h.check_i1();
}

#[test]
fn an_unanswered_dispute_expires_paying_the_worker_and_returning_the_bond() {
    let (mut h, client, worker) = market();
    let id = disputed(&mut h, client, worker);
    h.advance(3_601);
    h.as_("anyone.test");
    h.c.expire_dispute(id);
    assert_eq!(h.balance(worker), 990_000);
    assert_eq!(h.balance(client), 9_000_000);
    let r = h.c.worker_record(worker).unwrap();
    assert_eq!((r.unresolved, r.reviews), (1, 0), "nobody is rated");
    h.check_i1();
}

#[test]
fn pause_stops_new_hires_but_never_an_exit() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 100_000);
    h.as_("mallory.test");
    expect_panic("NotPauser", || h.c.pause());
    h.as_("pauser.test");
    h.c.pause();
    h.as_("c.alice.test");
    expect_panic("Paused", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 1_800);
    });
    expect_panic("Paused", || {
        h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    });
    h.as_("w.bob.test");
    h.c.accept_job(id);
    h.c.submit_result(id, RESULT.into());
    h.as_("c.alice.test");
    h.c.approve(id, None);
    h.as_yocto("pauser.test");
    expect_panic("NotOwner", || h.c.unpause());
    h.as_yocto("owner.test");
    h.c.unpause();
}

#[test]
fn direct_pay_is_instant_and_capped() {
    let (mut h, client, worker) = market();
    h.as_("c.alice.test");
    expect_panic("AmountTooLargeForFastPath", || {
        h.c.direct_pay(client, worker, U128(30_001), SPEC.into());
    });
    let id = h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    assert_eq!(h.job(id).state, JobState::Settled);
    assert_eq!(h.balance(worker), 20_000 - 200);
    assert_eq!(h.c.get_totals().fees_owed.0, 200);
    h.check_i1();
}

#[test]
fn the_minimum_fee_applies_to_small_jobs() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 10_000);
    assert_eq!(h.job(id).fee.0, 100);
    assert_eq!(h.job(id).worker_bond.0, 10_000, "min bond floor");
}

// ─────────────────────────── money out ───────────────────────────

#[test]
fn withdraw_holds_the_amount_in_flight_and_restores_it_on_failure() {
    let (mut h, client, _) = market();
    h.as_yocto("alice.test");
    let _ = h.c.withdraw(client, U128(1_000_000), None);
    let t = h.c.get_totals();
    assert_eq!(t.in_flight.0, 1_000_000);
    h.check_i1();
    h.finish_send(false, "alice.test", 1_000_000, Refund::Purse(client));
    assert_eq!(h.balance(client), 10_000_000, "restored");
    assert_eq!(h.c.get_totals().in_flight.0, 0);
    h.check_i1();
    h.as_yocto("alice.test");
    let _ = h.c.withdraw(client, U128(1_000_000), None);
    h.finish_send(true, "alice.test", 1_000_000, Refund::Purse(client));
    assert_eq!(h.balance(client), 9_000_000);
    h.check_i1();
}

#[test]
fn fees_go_to_the_fee_recipient_and_come_back_if_the_transfer_fails() {
    let (mut h, client, worker) = market();
    h.as_("c.alice.test");
    h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    h.as_("anyone.test");
    let _ = h.c.claim_fees();
    assert_eq!(h.c.get_totals().fees_owed.0, 0);
    h.finish_send(false, "fees.test", 200, Refund::Fees);
    assert_eq!(h.c.get_totals().fees_owed.0, 200);
    h.check_i1();
    h.as_("anyone.test");
    expect_panic("NothingOwed", || {
        let mut h2 = H::new();
        h2.as_("anyone.test");
        let _ = h2.c.claim_fees();
    });
}

// `#[private]` is enforced in the wasm entry point, not in the Rust method,
// so `on_send` being private is proven in the sandbox suite
// (tests/sandbox.rs), against the real compiled contract.

#[test]
fn only_the_payment_token_can_fund() {
    let (mut h, client, _) = market();
    h.as_("fake-usdc.test");
    expect_panic("WrongToken", || {
        let _ = h.c.ft_on_transfer(
            acc("x.test"),
            U128(5),
            format!(r#"{{"Fund":{{"agent_id":{client}}}}}"#),
        );
    });
    h.as_(TOKEN);
    expect_panic("BadMessage", || {
        let _ =
            h.c.ft_on_transfer(acc("x.test"), U128(5), "nonsense".into());
    });
}

// ─────────────────────────── stake ───────────────────────────

#[test]
fn unstaking_waits_out_the_delay_and_never_touches_locked_stake() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 1_000_000);
    h.as_("w.bob.test");
    h.c.accept_job(id); // locks 200_000
    h.as_yocto("bob.test");
    expect_panic("InsufficientBond", || {
        h.c.request_unstake(worker, U128(19_800_001))
    });
    h.c.request_unstake(worker, U128(19_800_000));
    assert!(!h.c.is_hireable(worker));
    expect_panic("WithdrawAlreadyPending", || {
        h.c.request_unstake(worker, U128(1))
    });
    expect_panic("StillLocked", || h.c.complete_unstake(worker));
    h.advance(604_800);
    h.as_yocto("bob.test");
    h.c.complete_unstake(worker);
    assert_eq!(h.balance(worker), 19_800_000);
    h.check_i1();
}

// ─────────────────────────── params ───────────────────────────

#[test]
fn params_are_bounded() {
    let mut h = H::new();
    h.as_yocto("owner.test");
    let mut p = params();
    p.protocol_fee_bps = 1_001;
    expect_panic("FeeTooHigh", || h.c.set_params(p.clone()));
    let mut p = params();
    p.review_window = 0;
    expect_panic("InvalidWindow", || h.c.set_params(p.clone()));
    let mut p = params();
    p.min_fee = U128(10_000);
    expect_panic("InvalidFeeFloor", || h.c.set_params(p.clone()));
    let mut p = params();
    p.bond_bps = 10_001;
    expect_panic("InvalidBond", || h.c.set_params(p.clone()));
    let mut p = params();
    p.min_stake = U128(0);
    expect_panic("InvalidStake", || h.c.set_params(p.clone()));
    h.as_yocto("alice.test");
    expect_panic("NotOwner", || h.c.set_params(params()));
}

#[test]
fn ownership_moves_in_two_steps() {
    let mut h = H::new();
    h.as_yocto("owner.test");
    h.c.propose_owner(acc("new.test"));
    h.as_yocto("mallory.test");
    expect_panic("NotPendingOwner", || h.c.accept_owner());
    h.as_yocto("new.test");
    h.c.accept_owner();
    assert_eq!(h.c.get_roles().owner, acc("new.test"));
}

// ─────────────────────────── invariants (property test) ───────────────────────────

mod invariants {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Create(u128),
        Direct(u128),
        Accept,
        Submit,
        Approve,
        Dispute,
        Rule(bool),
        Cancel,
        Expire,
        Advance(u64),
        Withdraw(u128, bool),
        Unstake(u128),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (5_000u128..3_000_000).prop_map(Op::Create),
            (5_000u128..40_000).prop_map(Op::Direct),
            Just(Op::Accept),
            Just(Op::Submit),
            Just(Op::Approve),
            Just(Op::Dispute),
            any::<bool>().prop_map(Op::Rule),
            Just(Op::Cancel),
            Just(Op::Expire),
            (1u64..8_000).prop_map(Op::Advance),
            ((1u128..2_000_000), any::<bool>()).prop_map(|(a, ok)| Op::Withdraw(a, ok)),
            (1u128..5_000_000).prop_map(Op::Unstake),
        ]
    }

    fn try_(f: impl FnOnce()) {
        let _ = catch_unwind(AssertUnwindSafe(f));
    }

    /// I1, I1b, I4, I6, I11, I12 from the v3 invariant suite.
    fn check(h: &H, jobs: &[JobId], worker: AgentId) {
        h.check_i1();
        let t = h.c.get_totals();
        let (mut locked, mut bonds, mut dispute_bonds) = (0u128, 0u128, 0u128);
        for id in jobs {
            let j = h.job(*id);
            assert!(j.fee.0 < j.amount.0, "I4");
            assert_ne!(j.client, j.worker, "I6");
            if j.state.is_open() {
                locked += j.amount.0;
            }
            if j.bond_locked {
                assert!(matches!(
                    j.state,
                    JobState::Accepted | JobState::Submitted | JobState::Disputed
                ));
                bonds += j.worker_bond.0;
            }
            if j.state == JobState::Disputed {
                dispute_bonds += j.dispute_bond.0;
                assert!(j.dispute_deadline > 0);
            }
        }
        assert_eq!(t.locked.0, locked, "I1b");
        assert_eq!(h.c.stake_of(worker).unwrap().locked.0, bonds, "I11");
        assert_eq!(t.dispute_bonds.0, dispute_bonds, "I12");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn invariants_hold_after_every_step(ops in prop::collection::vec(op(), 1..60)) {
            let (mut h, client, worker) = market();
            let mut jobs: Vec<JobId> = vec![];
            for op in ops {
                let last = jobs.last().copied();
                match op {
                    Op::Create(a) => {
                        h.as_("c.alice.test");
                        let r = catch_unwind(AssertUnwindSafe(|| {
                            h.c.create_job(client, worker, U128(a), SPEC.into(), 300, 1_800)
                        }));
                        if let Ok(id) = r { jobs.push(id); }
                    }
                    Op::Direct(a) => {
                        h.as_("c.alice.test");
                        let r = catch_unwind(AssertUnwindSafe(|| {
                            h.c.direct_pay(client, worker, U128(a), SPEC.into())
                        }));
                        if let Ok(id) = r { jobs.push(id); }
                    }
                    Op::Accept => if let Some(id) = last { h.as_("w.bob.test"); try_(|| h.c.accept_job(id)); },
                    Op::Submit => if let Some(id) = last { h.as_("w.bob.test"); try_(|| h.c.submit_result(id, RESULT.into())); },
                    Op::Approve => if let Some(id) = last { h.as_("c.alice.test"); try_(|| h.c.approve(id, None)); },
                    Op::Dispute => if let Some(id) = last { h.as_("c.alice.test"); try_(|| h.c.dispute(id, None)); },
                    Op::Rule(w) => if let Some(id) = last { h.as_("dao.test"); try_(|| h.c.resolve_dispute(id, w)); },
                    Op::Cancel => if let Some(id) = last { h.as_("c.alice.test"); try_(|| h.c.cancel(id)); },
                    Op::Expire => {
                        for id in jobs.clone() {
                            h.as_("anyone.test");
                            try_(|| h.c.expire_unaccepted(id));
                            try_(|| h.c.expire_undelivered(id));
                            try_(|| h.c.auto_approve(id));
                            try_(|| h.c.expire_dispute(id));
                        }
                    }
                    Op::Advance(s) => h.advance(s),
                    Op::Withdraw(a, ok) => {
                        h.as_yocto("bob.test");
                        let r = catch_unwind(AssertUnwindSafe(|| { let _ = h.c.withdraw(worker, U128(a), None); }));
                        if r.is_ok() { h.finish_send(ok, "bob.test", a, Refund::Purse(worker)); }
                    }
                    Op::Unstake(a) => {
                        h.as_yocto("bob.test");
                        try_(|| h.c.request_unstake(worker, U128(a)));
                        try_(|| h.c.complete_unstake(worker));
                    }
                }
                check(&h, &jobs, worker);
            }
        }
    }
}
