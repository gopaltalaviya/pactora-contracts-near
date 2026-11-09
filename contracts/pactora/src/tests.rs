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
        dispute_grace: 86_400,
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
    h.c.propose_wallet(client, Some(acc("c2.alice.test")));
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
    expect_panic("InvalidWindow", || {
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

/// A stolen worker wallet key must not hand the stake away as bonds: it
/// could accept jobs from an accomplice's client and never deliver, and
/// expire_undelivered would pay every bond to that client. Bonds the wallet
/// locks count toward the worker's own caps, so the most it can lose is them.
#[test]
fn a_worker_wallet_cannot_lock_bonds_beyond_its_caps() {
    let (mut h, client, worker) = market();
    h.as_yocto("bob.test");
    h.c.set_caps(worker, U128(100_000), U128(300_000));

    let big = create(&mut h, client, worker, 1_000_000); // bond 200_000
    h.as_("w.bob.test");
    expect_panic("PerTaskCapExceeded", || h.c.accept_job(big));
    assert_eq!(h.c.stake_of(worker).unwrap().locked.0, 0);

    // Three bonds of 80_000 fit the 300_000 day; the fourth does not.
    for _ in 0..3 {
        let id = create(&mut h, client, worker, 400_000);
        h.as_("w.bob.test");
        h.c.accept_job(id);
    }
    assert_eq!(h.c.stake_of(worker).unwrap().locked.0, 240_000);
    let fourth = create(&mut h, client, worker, 400_000);
    h.as_("w.bob.test");
    expect_panic("DailyCapExceeded", || h.c.accept_job(fourth));
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
    expect_panic("StillLocked", || {
        let _ = h.c.complete_unstake(worker);
    });
    h.advance(604_800);
    h.as_yocto("bob.test");
    let _ = h.c.complete_unstake(worker);
    // Paid to the owner, never through the agent's spendable balance.
    assert_eq!(h.balance(worker), 0);
    assert_eq!(h.c.get_totals().in_flight.0, 19_800_000);
    h.check_i1();
    h.finish_send(true, "bob.test", 19_800_000, Refund::Stake(worker));
    h.check_i1();
}

#[test]
fn a_failed_unstake_payout_goes_back_into_stake() {
    let (mut h, _, worker) = market();
    h.as_yocto("bob.test");
    h.c.request_unstake(worker, U128(5_000_000));
    h.advance(604_800);
    h.as_yocto("bob.test");
    let _ = h.c.complete_unstake(worker);
    h.finish_send(false, "bob.test", 5_000_000, Refund::Stake(worker));
    let s = h.c.stake_of(worker).unwrap();
    assert_eq!((s.available.0, s.pending.0), (20_000_000, 0));
    h.check_i1();
}

#[test]
fn cancel_unstake_restores_the_stake() {
    let (mut h, _, worker) = market();
    h.as_yocto("bob.test");
    expect_panic("NothingPending", || h.c.cancel_unstake(worker));
    h.c.request_unstake(worker, U128(15_000_000));
    assert!(!h.c.is_hireable(worker));
    h.c.cancel_unstake(worker);
    let s = h.c.stake_of(worker).unwrap();
    assert_eq!(
        (s.available.0, s.pending.0, s.unlock_at),
        (20_000_000, 0, 0)
    );
    assert!(h.c.is_hireable(worker));
    h.check_i1();
}

// ─────────────────────────── kill switch ───────────────────────────

#[test]
fn a_frozen_agent_wallet_can_do_nothing_but_exits_still_work() {
    let (mut h, client, worker) = market();
    let open = create(&mut h, client, worker, 100_000);
    let submitted = create(&mut h, client, worker, 100_000);
    h.as_("w.bob.test");
    h.c.accept_job(submitted);
    h.c.submit_result(submitted, RESULT.into());

    h.as_("alice.test");
    h.c.freeze_agent(client);
    h.as_("c.alice.test");
    expect_panic("AgentFrozen", || {
        h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 1_800);
    });
    expect_panic("AgentFrozen", || {
        h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    });
    expect_panic("AgentFrozen", || h.c.cancel(open));
    expect_panic("AgentFrozen", || h.c.approve(submitted, None));
    expect_panic("AgentFrozen", || h.c.dispute(submitted, None));

    h.as_("bob.test");
    h.c.freeze_agent(worker);
    h.as_("w.bob.test");
    expect_panic("AgentFrozen", || h.c.accept_job(open));

    // Exits anyone may call still end both jobs.
    h.advance(601);
    h.as_("anyone.test");
    h.c.expire_unaccepted(open);
    h.c.auto_approve(submitted);
    h.check_i1();

    // Unfrozen, both act again.
    h.as_yocto("alice.test");
    h.c.unfreeze_agent(client);
    h.as_yocto("bob.test");
    h.c.unfreeze_agent(worker);
    h.as_("c.alice.test");
    h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    h.check_i1();
}

// ─────────────────────────── storage ───────────────────────────

fn storage_bytes() -> u64 {
    near_sdk::env::storage_usage()
}

/// Bytes one call adds, measured inside that call. (The mocked runtime
/// resets its storage counter whenever the caller changes, so storage can
/// only be compared within one call, which is also how the contract bills.)
fn bytes_added_by(h: &mut H, who: &str, f: impl FnOnce(&mut Contract)) -> u64 {
    h.as_(who);
    h.c.flush();
    let before = storage_bytes();
    f(&mut h.c);
    h.c.flush();
    storage_bytes().saturating_sub(before)
}

/// What a job grows by after `create_job` billed it must fit in the reserve
/// billed up front, on the path that grows it most (result, reason, ruling)
/// and on the approve path (result, review).
#[test]
fn later_job_growth_fits_the_reserve_billed_at_creation() {
    let (mut h, client, worker) = market();
    let id = create(&mut h, client, worker, 1_000_000);
    let mut growth = bytes_added_by(&mut h, "w.bob.test", |c| c.accept_job(id));
    growth += bytes_added_by(&mut h, "w.bob.test", |c| c.submit_result(id, RESULT.into()));
    growth += bytes_added_by(&mut h, "c.alice.test", |c| c.dispute(id, Some(SPEC.into())));
    growth += bytes_added_by(&mut h, "dao.test", |c| c.resolve_dispute(id, false));
    assert!(
        growth > 0 && growth <= crate::JOB_GROWTH_BYTES,
        "dispute path grew {growth} bytes"
    );

    let id = create(&mut h, client, worker, 1_000_000);
    let mut growth = bytes_added_by(&mut h, "w.bob.test", |c| c.accept_job(id));
    growth += bytes_added_by(&mut h, "w.bob.test", |c| c.submit_result(id, RESULT.into()));
    growth += bytes_added_by(&mut h, "c.alice.test", |c| c.approve(id, Some(SPEC.into())));
    assert!(
        growth > 0 && growth <= crate::JOB_GROWTH_BYTES,
        "approve path grew {growth} bytes"
    );
}

/// Creation itself is billed to the client, measured in the same call.
#[test]
fn creating_a_job_is_billed_to_the_client() {
    let (mut h, client, worker) = market();
    let b0 = h.c.storage_balance_of(client).0;
    let added = bytes_added_by(&mut h, "c.alice.test", |c| {
        c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 1_800);
    });
    let billed = b0 - h.c.storage_balance_of(client).0;
    let cost = near_sdk::env::storage_byte_cost().as_yoctonear();
    assert_eq!(billed, (added + crate::JOB_GROWTH_BYTES) as u128 * cost);
}

#[test]
fn growing_an_agent_record_is_billed_to_the_agent() {
    let (mut h, client, _) = market();
    let b0 = h.c.storage_balance_of(client).0;
    h.as_yocto("alice.test");
    h.c.set_card_uri(client, "x".repeat(512));
    let b1 = h.c.storage_balance_of(client).0;
    assert!(b1 < b0, "a longer card is billed");
    h.c.propose_wallet(client, Some(acc("c2.alice.test")));
    let b2 = h.c.storage_balance_of(client).0;
    assert!(b2 < b1, "a pending wallet is billed");
    h.as_("c2.alice.test");
    h.c.accept_wallet(client);
    assert!(
        h.c.storage_balance_of(client).0 <= b2,
        "the wallet index is billed"
    );
}

#[test]
fn growth_is_refused_when_the_agent_has_no_storage_left() {
    let mut h = H::new();
    ctx("alice.test", NEAR, h.now);
    let id =
        h.c.register_agent(acc("c.alice.test"), "".into(), None, None);
    let left = h.c.storage_balance_of(id).0;
    h.as_yocto("alice.test");
    let _ = h.c.storage_withdraw(id, U128(left));
    assert_eq!(h.c.storage_balance_of(id).0, 0);
    expect_panic("InsufficientStorage", || {
        h.c.set_card_uri(id, "x".repeat(512))
    });
}

#[test]
fn storage_can_be_topped_up_and_withdrawn() {
    let (mut h, client, _) = market();
    let b0 = h.c.storage_balance_of(client).0;
    ctx("anyone.test", NEAR, h.now);
    assert_eq!(h.c.storage_deposit(client).0, b0 + NEAR);
    h.as_yocto("c.alice.test");
    expect_panic("NotAgentOwner", || {
        let _ = h.c.storage_withdraw(client, U128(1));
    });
    h.as_yocto("alice.test");
    expect_panic("InsufficientStorage", || {
        let _ = h.c.storage_withdraw(client, U128(b0 + NEAR + 1));
    });
    let _ = h.c.storage_withdraw(client, U128(NEAR));
    assert_eq!(h.c.storage_balance_of(client).0, b0);
}

#[test]
fn a_failed_storage_withdrawal_restores_the_balance() {
    let (mut h, client, _) = market();
    let b0 = h.c.storage_balance_of(client).0;
    h.as_yocto("alice.test");
    let _ = h.c.storage_withdraw(client, U128(1_000));
    let mut b = VMContextBuilder::new();
    b.current_account_id(acc(CONTRACT))
        .predecessor_account_id(acc(CONTRACT));
    testing_env!(
        b.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Failed]
    );
    assert!(!h.c.on_storage_sent(client, U128(1_000)));
    assert_eq!(h.c.storage_balance_of(client).0, b0);
}

// ─────────────────────────── wallets, gas, verdicts, order ───────────────────────────

#[test]
fn a_wallet_proposal_can_be_withdrawn() {
    let (mut h, client, _) = market();
    h.as_yocto("alice.test");
    h.c.propose_wallet(client, Some(acc("typo.test")));
    h.c.propose_wallet(client, None);
    h.as_("typo.test");
    expect_panic("NotPendingWallet", || h.c.accept_wallet(client));
    assert_eq!(
        h.c.get_agent(client).unwrap().wallet,
        Some(acc("c.alice.test"))
    );
}

#[test]
fn sending_needs_gas_left_and_changes_nothing_without_it() {
    let (mut h, client, _) = market();
    let mut b = VMContextBuilder::new();
    b.current_account_id(acc(CONTRACT))
        .predecessor_account_id(acc("alice.test"))
        .attached_deposit(NearToken::from_yoctonear(1))
        .block_timestamp(h.now * 1_000_000_000)
        .prepaid_gas(Gas::from_tgas(20));
    testing_env!(b.build());
    expect_panic("NotEnoughGas", || {
        let _ = h.c.withdraw(client, U128(1_000), None);
    });
    assert_eq!(h.balance(client), 10_000_000);
}

#[test]
fn verdicts_are_given_only_for_attributable_endings() {
    let (mut h, client, worker) = market();
    // Direct pay: no result was delivered.
    h.as_("c.alice.test");
    let direct = h.c.direct_pay(client, worker, U128(20_000), SPEC.into());
    assert_eq!(h.c.verdict_of(direct), None);
    // A dispute that expires: settled, but not attributable.
    let id = disputed(&mut h, client, worker);
    h.advance(3_600 + 86_400 + 1);
    h.as_("anyone.test");
    h.c.expire_dispute(id);
    assert_eq!(h.c.verdict_of(id), None);
    // An open job and an unknown job.
    let open = create(&mut h, client, worker, 100_000);
    assert_eq!(h.c.verdict_of(open), None);
    assert_eq!(h.c.verdict_of(9_999), None);
}

#[test]
fn same_owner_is_refused_before_hireability() {
    let mut h = H::new();
    let a = h.agent("alice.test", "a1.alice.test");
    let b = h.agent("alice.test", "a2.alice.test"); // not staked
    h.fund(a, 1_000_000);
    h.as_("a1.alice.test");
    expect_panic("SameOwner", || {
        h.c.create_job(a, b, U128(100_000), SPEC.into(), 300, 1_800);
    });
}

#[test]
fn zero_bonds_emit_no_bond_events() {
    let mut h = H::new();
    h.as_yocto("owner.test");
    let mut p = params();
    p.dispute_bond_bps = 0;
    p.min_dispute_bond = U128(0);
    h.c.set_params(p);
    let client = h.agent("alice.test", "c.alice.test");
    let worker = h.agent("bob.test", "w.bob.test");
    h.fund(client, 10_000_000);
    h.stake(worker, 20_000_000);
    let id = disputed(&mut h, client, worker);
    h.as_("dao.test");
    h.c.resolve_dispute(id, true);
    let logs = near_sdk::test_utils::get_logs();
    assert!(
        !logs.iter().any(|l| l.contains("dispute_bond_settled")),
        "{logs:?}"
    );
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
    p.dispute_grace = 0;
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
        ClaimFees(bool),
        Unstake(u128, bool),
        Freeze(bool),
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
            any::<bool>().prop_map(Op::ClaimFees),
            ((1u128..5_000_000), any::<bool>()).prop_map(|(a, ok)| Op::Unstake(a, ok)),
            any::<bool>().prop_map(Op::Freeze),
        ]
    }

    /// Storage, contract state and the token model, saved before a call.
    struct Snapshot {
        storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
        contract: Vec<u8>,
        held: u128,
    }

    impl H {
        fn snapshot(&mut self) -> Snapshot {
            self.c.flush();
            let storage = near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
            let copy = storage.clone();
            self.install(storage);
            Snapshot {
                storage: copy,
                contract: near_sdk::borsh::to_vec(&self.c).unwrap(),
                held: self.held,
            }
        }

        fn install(&self, storage: std::collections::HashMap<Vec<u8>, Vec<u8>>) {
            let mut b = VMContextBuilder::new();
            b.current_account_id(acc(CONTRACT))
                .block_timestamp(self.now * 1_000_000_000);
            near_sdk::env::set_blockchain_interface(near_sdk::MockedBlockchain::new(
                b.build(),
                near_sdk::test_vm_config(),
                near_sdk::RuntimeFeesConfig::test(),
                vec![],
                storage,
                Default::default(),
                None,
            ));
        }

        /// A panic rolls the whole call back, as the NEAR runtime does: the
        /// half-changed contract is forgotten (not dropped — dropping would
        /// flush its cached writes) and storage is put back.
        fn restore(&mut self, snap: Snapshot) {
            let restored: Contract = near_sdk::borsh::from_slice(&snap.contract).unwrap();
            let broken = std::mem::replace(&mut self.c, restored);
            std::mem::forget(broken);
            self.install(snap.storage);
            self.held = snap.held;
        }

        /// Call as `who` (with one yocto if `yocto`), atomically.
        fn call<R>(
            &mut self,
            who: &str,
            yocto: bool,
            f: impl FnOnce(&mut Contract) -> R,
        ) -> Option<R> {
            let snap = self.snapshot();
            ctx(who, yocto as u128, self.now);
            match catch_unwind(AssertUnwindSafe(|| f(&mut self.c))) {
                Ok(r) => Some(r),
                Err(_) => {
                    self.restore(snap);
                    None
                }
            }
        }
    }

    /// I1, I1b, I4, I6, I10, I11 and I12 from the v3 invariant suite.
    fn check(h: &H, jobs: &[JobId], worker: AgentId) {
        h.check_i1();
        let t = h.c.get_totals();
        let (mut locked, mut bonds, mut dispute_bonds) = (0u128, 0u128, 0u128);
        let (mut completed, mut unresolved, mut failed, mut lost) = (0u64, 0u64, 0u64, 0u64);
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
            match (j.state, j.outcome) {
                (JobState::Settled, Some(Outcome::Success)) => completed += 1,
                (JobState::Settled, Some(Outcome::Unresolved)) => unresolved += 1,
                (JobState::Refunded, Some(Outcome::Failed)) => failed += 1,
                (JobState::Refunded, Some(Outcome::DisputeLost)) => lost += 1,
                (JobState::Settled, other) => panic!("settled with outcome {other:?}"),
                _ => {}
            }
        }
        assert_eq!(t.locked.0, locked, "I1b");
        assert_eq!(h.c.stake_of(worker).unwrap().locked.0, bonds, "I11");
        assert_eq!(t.dispute_bonds.0, dispute_bonds, "I12");
        let r = h.c.worker_record(worker).unwrap();
        assert_eq!(
            (r.completed, r.unresolved, r.failed, r.disputes_lost),
            (completed, unresolved, failed, lost),
            "I10: records match the jobs that ended"
        );
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
                        if let Some(id) = h.call("c.alice.test", false, |c| {
                            c.create_job(client, worker, U128(a), SPEC.into(), 300, 1_800)
                        }) { jobs.push(id); }
                    }
                    Op::Direct(a) => {
                        if let Some(id) = h.call("c.alice.test", false, |c| {
                            c.direct_pay(client, worker, U128(a), SPEC.into())
                        }) { jobs.push(id); }
                    }
                    Op::Accept => if let Some(id) = last { h.call("w.bob.test", false, |c| c.accept_job(id)); },
                    Op::Submit => if let Some(id) = last { h.call("w.bob.test", false, |c| c.submit_result(id, RESULT.into())); },
                    Op::Approve => if let Some(id) = last { h.call("c.alice.test", false, |c| c.approve(id, None)); },
                    Op::Dispute => if let Some(id) = last { h.call("c.alice.test", false, |c| c.dispute(id, None)); },
                    Op::Rule(w) => if let Some(id) = last { h.call("dao.test", false, |c| c.resolve_dispute(id, w)); },
                    Op::Cancel => if let Some(id) = last { h.call("c.alice.test", false, |c| c.cancel(id)); },
                    Op::Expire => {
                        for id in jobs.clone() {
                            h.call("anyone.test", false, |c| c.expire_unaccepted(id));
                            h.call("anyone.test", false, |c| c.expire_undelivered(id));
                            h.call("anyone.test", false, |c| c.auto_approve(id));
                            h.call("anyone.test", false, |c| c.expire_dispute(id));
                        }
                    }
                    Op::Advance(s) => h.advance(s),
                    Op::Withdraw(a, ok) => {
                        if h.call("bob.test", true, |c| { let _ = c.withdraw(worker, U128(a), None); }).is_some() {
                            h.finish_send(ok, "bob.test", a, Refund::Purse(worker));
                        }
                    }
                    Op::ClaimFees(ok) => {
                        let owed = h.c.get_totals().fees_owed.0;
                        if h.call("anyone.test", false, |c| { let _ = c.claim_fees(); }).is_some() {
                            h.finish_send(ok, "fees.test", owed, Refund::Fees);
                        }
                    }
                    Op::Unstake(a, ok) => {
                        h.call("bob.test", true, |c| c.request_unstake(worker, U128(a)));
                        let pending = h.c.stake_of(worker).unwrap().pending.0;
                        if h.call("bob.test", true, |c| { let _ = c.complete_unstake(worker); }).is_some() {
                            h.finish_send(ok, "bob.test", pending, Refund::Stake(worker));
                        }
                    }
                    Op::Freeze(on) => {
                        if on {
                            h.call("alice.test", false, |c| c.freeze_agent(client));
                        } else {
                            h.call("alice.test", true, |c| c.unfreeze_agent(client));
                        }
                    }
                }
                check(&h, &jobs, worker);
            }
        }
    }
}

// ─────────────────────────── audit fixes (2026-11-09) ───────────────────────────
// Each test here was run against the code before its fix and failed.

mod audit {
    use super::*;
    use crate::migration;

    const GRACE: u64 = 86_400;

    fn logs() -> Vec<String> {
        near_sdk::test_utils::get_logs()
    }

    /// The arbiter may rule until the expiry actually executes, and nobody may
    /// expire a dispute inside the grace after its deadline — so a late
    /// ruling can no longer be raced by the worker calling `expire_dispute`
    /// in the same block.
    #[test]
    fn a_ruling_after_the_deadline_wins_and_expiry_waits_out_the_grace() {
        let (mut h, client, worker) = market();
        let ruled = disputed(&mut h, client, worker);
        let expired = disputed(&mut h, client, worker);
        let deadline = h.job(ruled).dispute_deadline;
        h.now = deadline + 1;

        h.as_("anyone.test");
        expect_panic("DeadlineNotPassed", || h.c.expire_dispute(expired));
        h.as_("dao.test");
        h.c.resolve_dispute(ruled, false);
        assert_eq!(h.job(ruled).state, JobState::Refunded);

        h.now = deadline + GRACE;
        h.as_("anyone.test");
        expect_panic("DeadlineNotPassed", || h.c.expire_dispute(expired));
        h.now = deadline + GRACE + 1;
        // Still rulable right up to the moment someone expires it.
        let late = disputed(&mut h, client, worker);
        h.as_("dao.test");
        h.c.resolve_dispute(expired, true);
        assert_eq!(h.job(expired).state, JobState::Settled);
        h.now = h.job(late).dispute_deadline + GRACE + 1;
        h.as_("anyone.test");
        h.c.expire_dispute(late);
        assert_eq!(h.job(late).outcome, Some(Outcome::Unresolved));
        h.as_("dao.test");
        expect_panic("InvalidState", || h.c.resolve_dispute(late, false));
        h.check_i1();
    }

    /// The dispute bond is refunded unless the dispute is ruled against, so
    /// it checks only the balance, never the per-task or daily caps: a client
    /// that spent its whole day on the hire can still dispute the result.
    #[test]
    fn a_client_at_its_daily_cap_can_still_dispute() {
        let (mut h, client, worker) = market();
        h.as_yocto("alice.test");
        h.c.set_caps(client, U128(1_000_000), U128(1_000_000));
        let id = create(&mut h, client, worker, 1_000_000);
        h.as_("w.bob.test");
        h.c.accept_job(id);
        h.c.submit_result(id, RESULT.into());
        h.as_("c.alice.test");
        h.c.dispute(id, None);
        assert_eq!(h.job(id).state, JobState::Disputed);
        assert_eq!(h.balance(client), 10_000_000 - 1_000_000 - 100_000);
        assert_eq!(h.c.get_totals().dispute_bonds.0, 100_000);
        // The bond did not count toward the window either.
        assert_eq!(h.c.purse_of(client).unwrap().spent_today.0, 1_000_000);
        h.check_i1();
    }

    /// The balance is still the floor for the bond.
    #[test]
    fn a_dispute_bond_above_the_balance_is_refused() {
        let (mut h, client, worker) = market();
        let id = create(&mut h, client, worker, 1_000_000);
        h.as_("w.bob.test");
        h.c.accept_job(id);
        h.c.submit_result(id, RESULT.into());
        h.as_yocto("alice.test");
        let _ = h.c.withdraw(client, U128(8_950_000), None);
        h.finish_send(true, "alice.test", 8_950_000, Refund::Purse(client));
        h.as_("c.alice.test");
        expect_panic("InsufficientBalance", || h.c.dispute(id, None));
        h.check_i1();
    }

    /// An arbiter that never rules costs the client half its bond: the worker
    /// waited through the review window and the dispute for nothing, and is
    /// paid the job plus that half. Nobody is rated.
    #[test]
    fn an_unanswered_dispute_splits_the_bond_between_worker_and_client() {
        let (mut h, client, worker) = market();
        let id = disputed(&mut h, client, worker);
        h.now = h.job(id).dispute_deadline + GRACE + 1;
        h.as_("anyone.test");
        h.c.expire_dispute(id);
        assert_eq!(
            h.balance(worker),
            990_000 + 50_000,
            "the job and half the bond"
        );
        assert_eq!(
            h.balance(client),
            10_000_000 - 1_000_000 - 100_000 + 50_000,
            "half the bond back"
        );
        assert_eq!(h.c.get_totals().dispute_bonds.0, 0);
        let r = h.c.worker_record(worker).unwrap();
        assert_eq!(
            (r.unresolved, r.completed, r.reviews),
            (1, 0, 0),
            "nobody is rated"
        );
        assert_eq!(h.c.client_record(client).unwrap().reviews, 0);
        let l = logs();
        let expired = l
            .iter()
            .find(|l| l.contains("\"dispute_expired\""))
            .expect("dispute_expired");
        assert!(
            expired.contains(r#""bond_to_worker":"50000""#)
                && expired.contains(r#""bond_to_client":"50000""#),
            "{expired}"
        );
        assert_eq!(
            l.iter()
                .filter(|l| l.contains("dispute_bond_settled"))
                .count(),
            2
        );
        h.check_i1();
    }

    /// A zero window is an invalid window, not a window that is too long.
    #[test]
    fn a_zero_window_is_an_invalid_window() {
        let (mut h, client, worker) = market();
        h.as_("c.alice.test");
        expect_panic("InvalidWindow", || {
            h.c.create_job(client, worker, U128(100_000), SPEC.into(), 0, 1_800);
        });
        expect_panic("InvalidWindow", || {
            h.c.create_job(client, worker, U128(100_000), SPEC.into(), 300, 0);
        });
    }

    /// `unstaked` is emitted when the payout lands, not when it is attempted;
    /// a payout that fails emits `stake_restored` instead.
    #[test]
    fn unstaked_is_emitted_only_when_the_payout_lands() {
        let (mut h, _, worker) = market();
        h.as_yocto("bob.test");
        h.c.request_unstake(worker, U128(5_000_000));
        h.advance(604_800);
        h.as_yocto("bob.test");
        let _ = h.c.complete_unstake(worker);
        assert!(
            !logs().iter().any(|l| l.contains("\"unstaked\"")),
            "{:?}",
            logs()
        );
        h.finish_send(false, "bob.test", 5_000_000, Refund::Stake(worker));
        let l = logs();
        assert!(l.iter().any(|l| l.contains("\"stake_restored\"")), "{l:?}");
        assert!(!l.iter().any(|l| l.contains("\"unstaked\"")), "{l:?}");
        assert_eq!(h.c.stake_of(worker).unwrap().available.0, 20_000_000);

        h.as_yocto("bob.test");
        h.c.request_unstake(worker, U128(5_000_000));
        h.advance(604_800);
        h.as_yocto("bob.test");
        let _ = h.c.complete_unstake(worker);
        h.finish_send(true, "bob.test", 5_000_000, Refund::Stake(worker));
        let l = logs();
        assert!(
            l.iter()
                .any(|l| l.contains("\"unstaked\"") && l.contains("\"5000000\"")),
            "{l:?}"
        );
        assert!(!l.iter().any(|l| l.contains("\"stake_restored\"")), "{l:?}");
        h.check_i1();
    }

    fn testnet_params() -> Params {
        Params {
            default_per_task_cap: U128(100_000),
            default_daily_cap: U128(1_000_000),
            ..params()
        }
    }

    /// Known limit (audit A2): a leaked client key, which cannot withdraw,
    /// can still burn the agent's prepaid storage NEAR with create/cancel
    /// loops. Each cycle costs the same, the tokens all come back, and the
    /// loop ends when the prepaid storage is gone — the owner's
    /// `freeze_agent` ends it sooner.
    #[test]
    fn a_leaked_client_key_can_only_burn_the_prepaid_storage() {
        let (mut h, client, worker) = market();
        let before = h.c.storage_balance_of(client).0;
        let mut costs = vec![];
        for _ in 0..20 {
            let b0 = h.c.storage_balance_of(client).0;
            h.as_("c.alice.test");
            let id =
                h.c.create_job(client, worker, U128(10_000), SPEC.into(), 300, 1_800);
            h.c.cancel(id);
            costs.push(b0 - h.c.storage_balance_of(client).0);
        }
        assert!(costs.iter().all(|c| *c > 0 && *c == costs[0]), "{costs:?}");
        assert_eq!(h.balance(client), 10_000_000, "every token came back");
        assert_eq!(before - h.c.storage_balance_of(client).0, 20 * costs[0]);
        // Frozen, the key can do nothing more.
        h.as_("alice.test");
        h.c.freeze_agent(client);
        h.as_("c.alice.test");
        expect_panic("AgentFrozen", || {
            h.c.create_job(client, worker, U128(10_000), SPEC.into(), 300, 1_800);
        });
        h.check_i1();
    }

    /// Known limit (audit A3): with the testnet default caps (per-task
    /// 100_000) a worker cannot accept a job above 500_000, since its 20%
    /// bond counts toward its own caps, although `create_job` allows up to
    /// `max_job_amount`. The owner raises the worker's caps with `set_caps`.
    #[test]
    fn default_caps_bound_the_bond_a_worker_can_lock() {
        let mut h = H::new();
        h.as_yocto("owner.test");
        h.c.set_params(testnet_params());
        let client = h.agent("alice.test", "c.alice.test");
        let worker = h.agent("bob.test", "w.bob.test");
        h.fund(client, 10_000_000);
        h.stake(worker, 20_000_000);
        h.as_yocto("alice.test");
        h.c.set_caps(client, U128(10_000_000), U128(10_000_000));
        let big = create(&mut h, client, worker, 1_000_000);
        h.as_("w.bob.test");
        expect_panic("PerTaskCapExceeded", || h.c.accept_job(big));
        let fits = create(&mut h, client, worker, 500_000);
        h.as_("w.bob.test");
        h.c.accept_job(fits);
        h.as_yocto("bob.test");
        h.c.set_caps(worker, U128(1_000_000), U128(1_000_000));
        h.as_("w.bob.test");
        h.c.accept_job(big);
        h.check_i1();
    }

    /// `recover_surplus` reaches only what lies above the totals — tokens a
    /// plain `ft_transfer` left in the contract — never what is owed.
    #[test]
    fn recover_surplus_takes_only_what_lies_above_the_totals() {
        let (mut h, _, _) = market();
        // A plain ft_transfer into the contract: held, credited to nobody.
        h.held += 777;
        h.as_yocto("alice.test");
        expect_panic("NotOwner", || {
            let _ = h.c.recover_surplus(U128(1), None);
        });
        h.as_("owner.test");
        expect_panic("Requires attached deposit of exactly 1 yoctoNEAR", || {
            let _ = h.c.recover_surplus(U128(1), None);
        });
        h.as_yocto("owner.test");
        let _ = h.c.recover_surplus(U128(1_000), None);
        let sum = h.c.get_totals().sum.0;
        assert_eq!(h.held, sum + 777);

        // The token answers with the real balance: only the 777 move.
        h.as_(CONTRACT);
        let _ =
            h.c.on_surplus_balance(Ok(U128(h.held)), U128(1_000), acc("owner.test"));
        assert_eq!(h.c.get_totals().in_flight.0, 777);
        assert!(logs()
            .iter()
            .any(|l| l.contains("\"surplus_recovered\"") && l.contains("\"777\"")));
        h.finish_send(true, "owner.test", 777, Refund::Surplus);
        assert_eq!(h.c.get_totals().sum.0, sum, "no bucket changed");
        h.check_i1();

        // Nothing above the totals now: refused, nothing moves.
        h.as_(CONTRACT);
        expect_panic("NoSurplus", || {
            let _ =
                h.c.on_surplus_balance(Ok(U128(h.held)), U128(1), acc("owner.test"));
        });
        expect_panic("BalanceUnknown", || {
            let _ = h.c.on_surplus_balance(
                Err(near_sdk::PromiseError::Failed),
                U128(1),
                acc("owner.test"),
            );
        });
        assert_eq!(h.c.get_totals().in_flight.0, 0);

        // A failed payout leaves the surplus where it was, still recoverable.
        h.held += 10;
        h.as_(CONTRACT);
        let _ =
            h.c.on_surplus_balance(Ok(U128(h.held)), U128(1_000), acc("owner.test"));
        assert_eq!(h.c.get_totals().in_flight.0, 10);
        h.finish_send(false, "owner.test", 10, Refund::Surplus);
        assert_eq!(h.c.get_totals().sum.0, sum);
        assert_eq!(h.held, sum + 10);
    }

    /// The state the 1.0.0 code left (no `version`, no `dispute_grace`)
    /// migrates to this layout with every view unchanged, and cannot be
    /// migrated twice.
    #[test]
    fn migrate_reads_the_1_0_0_layout_and_keeps_every_view() {
        let (mut h, client, worker) = market();
        let disputed_job = disputed(&mut h, client, worker);
        let open = create(&mut h, client, worker, 100_000);
        h.as_yocto("bob.test");
        h.c.request_unstake(worker, U128(1_000_000));
        h.as_("anyone.test");
        let views = |c: &Contract| {
            vec![
                format!("{:?}", c.get_roles()),
                format!("{:?}", c.get_agent(client)),
                format!("{:?}", c.get_agent(worker)),
                format!("{:?}", c.get_job(disputed_job)),
                format!("{:?}", c.get_job(open)),
                format!("{:?}", c.purse_of(client)),
                format!("{:?}", c.purse_of(worker)),
                format!("{:?}", c.stake_of(worker)),
                format!("{:?}", c.get_totals()),
                format!("{:?}", c.worker_record(worker)),
                format!("{:?}", c.client_record(client)),
                format!("{:?}", c.agent_count()),
                format!("{:?}", c.job_count()),
                format!("{:?}", c.storage_balance_of(client)),
                format!("{:?}", c.agent_of_wallet(acc("c.alice.test"))),
                format!("{:?}", c.verdict_of(disputed_job)),
                format!("{:?}", c.is_hireable(worker)),
            ]
        };
        let before = views(&h.c);
        let params_before = h.c.get_params();

        // What 1.0.0 would have written under STATE for this very state.
        h.c.flush();
        let p = &h.c.params;
        let v0 = migration::V0 {
            owner: h.c.owner.clone(),
            pending_owner: h.c.pending_owner.clone(),
            arbiter: h.c.arbiter.clone(),
            pauser: h.c.pauser.clone(),
            fee_recipient: h.c.fee_recipient.clone(),
            token: h.c.token.clone(),
            paused: h.c.paused,
            params: migration::ParamsV0 {
                protocol_fee_bps: p.protocol_fee_bps,
                min_fee: p.min_fee,
                min_job_amount: p.min_job_amount,
                max_job_amount: p.max_job_amount,
                fast_path_max: p.fast_path_max,
                review_window: p.review_window,
                max_accept_window: p.max_accept_window,
                max_work_window: p.max_work_window,
                dispute_timeout: p.dispute_timeout,
                bond_bps: p.bond_bps,
                min_bond: p.min_bond,
                dispute_bond_bps: p.dispute_bond_bps,
                min_dispute_bond: p.min_dispute_bond,
                min_stake: p.min_stake,
                withdraw_delay: p.withdraw_delay,
                default_per_task_cap: p.default_per_task_cap,
                default_daily_cap: p.default_daily_cap,
            },
            next_agent_id: h.c.next_agent_id,
            next_job_id: h.c.next_job_id,
            agents: near_sdk::store::LookupMap::new(crate::StorageKey::Agents),
            agent_by_wallet: near_sdk::store::LookupMap::new(crate::StorageKey::AgentByWallet),
            purses: near_sdk::store::LookupMap::new(crate::StorageKey::Purses),
            stakes: near_sdk::store::LookupMap::new(crate::StorageKey::Stakes),
            storage_balances: near_sdk::store::LookupMap::new(crate::StorageKey::StorageBalances),
            jobs: near_sdk::store::LookupMap::new(crate::StorageKey::Jobs),
            worker_records: near_sdk::store::LookupMap::new(crate::StorageKey::WorkerRecords),
            client_records: near_sdk::store::LookupMap::new(crate::StorageKey::ClientRecords),
            totals: h.c.totals.clone(),
        };
        near_sdk::env::state_write(&v0);
        let old_bytes = near_sdk::borsh::to_vec(&v0).unwrap();
        let new_bytes = near_sdk::borsh::to_vec(&h.c).unwrap();
        assert_ne!(old_bytes, new_bytes, "the layouts differ");
        assert!(
            <Contract as near_sdk::borsh::BorshDeserialize>::try_from_slice(&old_bytes).is_err(),
            "the new code cannot read 1.0.0 state as is"
        );

        ctx(CONTRACT, 0, h.now);
        let migrated = Contract::migrate();
        assert_eq!(migrated.version(), 1);
        assert_eq!(views(&migrated), before, "every view reads the same");
        let mut want = params_before.clone();
        want.dispute_grace = 86_400;
        assert_eq!(
            migrated.get_params(),
            want,
            "dispute_grace gets its default"
        );
        assert_eq!(migrated.params.dispute_grace, crate::DEFAULT_DISPUTE_GRACE);

        // A second migration finds no 1.0.0 state and changes nothing.
        near_sdk::env::state_write(&migrated);
        expect_panic("StateIsNotV0", || {
            let _ = Contract::migrate();
        });
        let again: Contract = near_sdk::env::state_read().unwrap();
        assert_eq!(views(&again), before);
        std::mem::forget(migrated);
        std::mem::forget(again);
    }
}
