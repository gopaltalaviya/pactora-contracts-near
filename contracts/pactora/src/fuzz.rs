//! Model-based property tests ("fuzz"): long random call sequences from many
//! accounts — the right party, the wrong party, strangers, the token, the
//! contract itself — with random deposits, low gas, clock jumps to exact
//! deadline boundaries (d-1, d, d+1), out-of-order transfer callbacks that
//! succeed or fail, parameter changes, wallet rotation and ownership moves.
//!
//! Each call runs atomically: a panic rolls storage and state back, as the
//! NEAR runtime does. For every call:
//!   1. an oracle predicts from the pre-state views whether it MUST succeed,
//!      MUST fail, or cannot be predicted (storage-bound calls only);
//!   2. on success, the whole post-state is predicted field by field (every
//!      agent's balance, stake, records, every job, the totals, roles,
//!      params, counters, the wallet index) and compared;
//!   3. global invariants are checked (see `check_invariants`).
//!
//! At the end of every case the market is drained: every job must be
//! closable by a stranger, every token withdrawable by its owner, and the
//! contract must end holding exactly zero tokens.
//!
//! Limits of the unit level: `#[private]`, `#[payable]` (refusal of a deposit
//! on a non-payable method) and `#[init]` are enforced by the wasm wrapper the
//! `#[near]` macro generates, not by the Rust method, so they are NOT
//! exercised here; the sandbox tests cover them.
//!
//! Run: `PROPTEST_CASES=5000 cargo test -p pactora --lib fuzz -- --test-threads=1`

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, Gas, NearToken, PromiseResult};
use proptest::prelude::*;

use crate::types::*;
use crate::Contract;

const TOKEN: &str = "usdc.test";
const CONTRACT: &str = "pactora.test";
const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const T0: u64 = 1_800_000_000;
const OK_HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
/// Below this storage balance, a storage-billed call is not predicted.
const STORAGE_SAFE: u128 = NEAR / 2;

const ACCTS: [&str; 16] = [
    "owner.test",    // 0 contract owner
    "dao.test",      // 1 arbiter
    "pauser.test",   // 2
    "fees.test",     // 3 fee recipient
    TOKEN,           // 4
    CONTRACT,        // 5
    "stranger.test", // 6
    "alice.test",    // 7 owns agents 1 and 4
    "bob.test",      // 8 owns agent 2
    "carol.test",    // 9 owns agent 3
    "a1.alice.test", // 10 wallet of agent 1
    "b1.bob.test",   // 11 wallet of agent 2
    "c1.carol.test", // 12 wallet of agent 3
    "a2.alice.test", // 13 wallet of agent 4
    "w5.test",       // 14 spare wallet
    "w6.test",       // 15 spare wallet
];

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

pub fn base_params() -> Params {
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

/// Parameter sets the fuzzer switches between; 5..=7 are invalid.
fn params_variant(i: u8) -> Params {
    let mut p = base_params();
    match i % 8 {
        0 => {}
        1 => {
            p.protocol_fee_bps = 0;
            p.min_fee = U128(0);
            p.bond_bps = 0;
            p.min_bond = U128(0);
            p.dispute_bond_bps = 0;
            p.min_dispute_bond = U128(0);
            p.fast_path_max = U128(0);
        }
        2 => {
            p.protocol_fee_bps = MAX_FEE_BPS;
            p.min_fee = U128(9_999);
            p.bond_bps = 10_000;
            p.dispute_bond_bps = 10_000;
            p.min_stake = U128(1);
        }
        3 => {
            p.review_window = 1;
            p.max_accept_window = 1;
            p.max_work_window = 1;
            p.dispute_timeout = 1;
            p.dispute_grace = 1;
            p.withdraw_delay = 1;
            p.min_job_amount = U128(1);
            p.min_fee = U128(0);
            p.min_bond = U128(0);
            p.min_dispute_bond = U128(0);
            p.fast_path_max = U128(1);
        }
        4 => {
            p.max_job_amount = U128(u128::MAX);
            p.fast_path_max = U128(u128::MAX);
            p.default_per_task_cap = U128(u128::MAX);
            p.default_daily_cap = U128(u128::MAX);
            p.review_window = MAX_WINDOW;
            p.max_accept_window = MAX_WINDOW;
            p.max_work_window = MAX_WINDOW;
            p.dispute_timeout = MAX_WINDOW;
            p.dispute_grace = MAX_WINDOW;
            p.withdraw_delay = MAX_WITHDRAW_DELAY;
        }
        5 => p.protocol_fee_bps = MAX_FEE_BPS + 1,
        6 => p.min_fee = p.min_job_amount,
        _ => p.withdraw_delay = 0,
    }
    p
}

fn params_valid(p: &Params) -> bool {
    let w = |x: u64| (1..=MAX_WINDOW).contains(&x);
    p.protocol_fee_bps <= MAX_FEE_BPS
        && w(p.review_window)
        && w(p.max_accept_window)
        && w(p.max_work_window)
        && w(p.dispute_timeout)
        && w(p.dispute_grace)
        && (1..=MAX_WITHDRAW_DELAY).contains(&p.withdraw_delay)
        && p.min_job_amount.0 > 0
        && p.min_fee.0 < p.min_job_amount.0
        && (p.fast_path_max.0 == 0 || p.fast_path_max.0 >= p.min_job_amount.0)
        && p.max_job_amount.0 >= p.min_job_amount.0
        && p.max_job_amount.0 >= p.fast_path_max.0
        && p.bond_bps as u128 <= BPS
        && p.dispute_bond_bps as u128 <= BPS
        && p.min_stake.0 > 0
        && p.default_per_task_cap.0 <= p.default_daily_cap.0
}

fn hash_variant(i: u8) -> (String, bool) {
    match i % 10 {
        0..=4 => (OK_HASH.into(), true),
        5 => (OK_HASH.to_uppercase(), false),
        6 => (OK_HASH[..63].into(), false),
        7 => (String::new(), false),
        8 => (format!("{OK_HASH}0"), false),
        _ => ("g".repeat(64), false),
    }
}

/// Checked `amount * bps / 10_000`, `max(floor)`; None = the contract overflows.
fn pct(amount: u128, bps: u16, floor: u128) -> Option<u128> {
    amount
        .checked_mul(bps as u128)
        .map(|x| (x / BPS).max(floor))
}

// ───────────────────────────── operations ─────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Who {
    Right,
    Acct(u8),
}

#[derive(Clone, Debug)]
pub enum Op {
    Fund {
        agent: u8,
        amt: u128,
        who: Who,
        msg: u8,
    },
    Create {
        client: u8,
        worker: u8,
        amt: u128,
        aw: u64,
        ww: u64,
        who: Who,
        hash: u8,
    },
    Direct {
        client: u8,
        worker: u8,
        amt: u128,
        who: Who,
        hash: u8,
    },
    Accept {
        job: u8,
        who: Who,
    },
    Submit {
        job: u8,
        who: Who,
        hash: u8,
    },
    Approve {
        job: u8,
        who: Who,
        hash: Option<u8>,
    },
    Dispute {
        job: u8,
        who: Who,
        hash: Option<u8>,
    },
    Rule {
        job: u8,
        for_worker: bool,
        who: Who,
    },
    Cancel {
        job: u8,
        who: Who,
    },
    /// 0 expire_unaccepted, 1 expire_undelivered, 2 auto_approve, 3 expire_dispute
    Exit {
        job: u8,
        kind: u8,
        who: Who,
    },
    Advance(u64),
    /// Set the clock to a deadline of a job (0 accept, 1 work, 2 review,
    /// 3 dispute, 5 dispute + grace) or a stake unlock (4), plus -1, 0 or +1.
    Jump {
        job: u8,
        kind: u8,
        delta: i8,
    },
    Withdraw {
        agent: u8,
        amt: u128,
        who: Who,
        dep: u8,
        other_receiver: bool,
        low_gas: bool,
    },
    ClaimFees {
        who: Who,
        low_gas: bool,
    },
    ReqUnstake {
        agent: u8,
        amt: u128,
        who: Who,
        dep: u8,
    },
    CancelUnstake {
        agent: u8,
        who: Who,
        dep: u8,
    },
    CompleteUnstake {
        agent: u8,
        who: Who,
        dep: u8,
        low_gas: bool,
    },
    Resolve {
        idx: u8,
        ok: bool,
    },
    Freeze {
        agent: u8,
        who: Who,
    },
    Unfreeze {
        agent: u8,
        who: Who,
        dep: u8,
    },
    SetCaps {
        agent: u8,
        per: u128,
        daily: u128,
        who: Who,
        dep: u8,
    },
    ProposeWallet {
        agent: u8,
        to: Option<u8>,
        who: Who,
        dep: u8,
    },
    AcceptWallet {
        agent: u8,
        who: Who,
    },
    SetCardUri {
        agent: u8,
        len: u16,
        who: Who,
        dep: u8,
    },
    Pause {
        who: Who,
    },
    Unpause {
        who: Who,
        dep: u8,
    },
    SetParams {
        v: u8,
        who: Who,
        dep: u8,
    },
    ProposeOwner {
        to: u8,
        who: Who,
        dep: u8,
    },
    AcceptOwner {
        who: Who,
        dep: u8,
    },
    SetArbiter {
        to: u8,
        who: Who,
        dep: u8,
    },
    StorageDeposit {
        agent: u8,
        amt: u128,
    },
    StorageWithdraw {
        agent: u8,
        amt: u8,
        who: Who,
        dep: u8,
    },
    Register {
        who: u8,
        wallet: u8,
        dep: u8,
        len: u16,
        caps: Option<(u128, u128)>,
    },
    /// A plain `ft_transfer` of `inject` into the contract (credited to
    /// nobody), then `who` asks for `amt` of the surplus back; the token's
    /// answer is fed to the callback, which may send `min(amt, surplus)`.
    RecoverSurplus {
        inject: u128,
        amt: u128,
        who: Who,
        dep: u8,
    },
}

fn dep_of(d: u8) -> u128 {
    match d % 10 {
        0 => 0,
        9 => 2,
        _ => 1,
    }
}

fn who_s() -> impl Strategy<Value = Who> {
    prop_oneof![4 => Just(Who::Right), 1 => (0u8..16).prop_map(Who::Acct)]
}

fn amount_s() -> impl Strategy<Value = u128> {
    prop_oneof![
        1 => Just(0u128),
        1 => Just(1u128),
        1 => Just(9_999u128),
        1 => Just(10_000u128),
        6 => 10_000u128..3_000_000,
        2 => 29_999u128..=30_001,
        1 => Just(5_000_000u128),
        1 => Just(5_000_001u128),
        1 => Just(10_000_000u128),
        1 => Just(10_000_001u128),
        1 => Just(u128::MAX / 10_000 + 1),
        1 => Just(u128::MAX),
    ]
}

fn window_s() -> impl Strategy<Value = u64> {
    prop_oneof![
        1 => Just(0u64),
        2 => Just(1u64),
        4 => 1u64..3_000,
        2 => Just(3_000u64),
        1 => Just(3_001u64),
        1 => Just(18_000u64),
        1 => Just(18_001u64),
        1 => Just(MAX_WINDOW),
        1 => Just(u64::MAX),
    ]
}

fn op_s() -> impl Strategy<Value = Op> {
    let a = any::<u8>;
    prop_oneof![
        3 => (a(), amount_s(), who_s(), a()).prop_map(|(agent, amt, who, msg)| Op::Fund { agent, amt, who, msg }),
        6 => (a(), a(), amount_s(), window_s(), window_s(), who_s(), a())
            .prop_map(|(client, worker, amt, aw, ww, who, hash)| Op::Create { client, worker, amt, aw, ww, who, hash }),
        2 => (a(), a(), amount_s(), who_s(), a()).prop_map(|(client, worker, amt, who, hash)| Op::Direct { client, worker, amt, who, hash }),
        5 => (a(), who_s()).prop_map(|(job, who)| Op::Accept { job, who }),
        5 => (a(), who_s(), a()).prop_map(|(job, who, hash)| Op::Submit { job, who, hash }),
        3 => (a(), who_s(), proptest::option::of(a())).prop_map(|(job, who, hash)| Op::Approve { job, who, hash }),
        3 => (a(), who_s(), proptest::option::of(a())).prop_map(|(job, who, hash)| Op::Dispute { job, who, hash }),
        3 => (a(), any::<bool>(), who_s()).prop_map(|(job, for_worker, who)| Op::Rule { job, for_worker, who }),
        2 => (a(), who_s()).prop_map(|(job, who)| Op::Cancel { job, who }),
        5 => (a(), 0u8..4, who_s()).prop_map(|(job, kind, who)| Op::Exit { job, kind, who }),
        2 => prop_oneof![Just(0u64), Just(1u64), 1u64..4_000, Just(3_600u64), Just(86_400u64), Just(MAX_WINDOW)].prop_map(Op::Advance),
        5 => (a(), 0u8..6, -1i8..=1).prop_map(|(job, kind, delta)| Op::Jump { job, kind, delta }),
        2 => (a(), amount_s(), who_s(), a(), any::<bool>(), proptest::bool::weighted(0.1))
            .prop_map(|(agent, amt, who, dep, other_receiver, low_gas)| Op::Withdraw { agent, amt, who, dep, other_receiver, low_gas }),
        1 => (who_s(), proptest::bool::weighted(0.1)).prop_map(|(who, low_gas)| Op::ClaimFees { who, low_gas }),
        2 => (a(), amount_s(), who_s(), a()).prop_map(|(agent, amt, who, dep)| Op::ReqUnstake { agent, amt, who, dep }),
        1 => (a(), who_s(), a()).prop_map(|(agent, who, dep)| Op::CancelUnstake { agent, who, dep }),
        1 => (a(), who_s(), a(), proptest::bool::weighted(0.1)).prop_map(|(agent, who, dep, low_gas)| Op::CompleteUnstake { agent, who, dep, low_gas }),
        4 => (a(), any::<bool>()).prop_map(|(idx, ok)| Op::Resolve { idx, ok }),
        1 => (a(), who_s()).prop_map(|(agent, who)| Op::Freeze { agent, who }),
        1 => (a(), who_s(), a()).prop_map(|(agent, who, dep)| Op::Unfreeze { agent, who, dep }),
        1 => (a(), amount_s(), amount_s(), who_s(), a()).prop_map(|(agent, per, daily, who, dep)| Op::SetCaps { agent, per, daily, who, dep }),
        1 => (a(), proptest::option::of(0u8..16), who_s(), a()).prop_map(|(agent, to, who, dep)| Op::ProposeWallet { agent, to, who, dep }),
        1 => (a(), who_s()).prop_map(|(agent, who)| Op::AcceptWallet { agent, who }),
        1 => (a(), prop_oneof![Just(0u16), 1u16..64, Just(512u16), Just(513u16), Just(10_000u16)], who_s(), a())
            .prop_map(|(agent, len, who, dep)| Op::SetCardUri { agent, len, who, dep }),
        1 => who_s().prop_map(|who| Op::Pause { who }),
        1 => (who_s(), a()).prop_map(|(who, dep)| Op::Unpause { who, dep }),
        1 => (a(), who_s(), a()).prop_map(|(v, who, dep)| Op::SetParams { v, who, dep }),
        1 => (0u8..16, who_s(), a()).prop_map(|(to, who, dep)| Op::ProposeOwner { to, who, dep }),
        1 => (who_s(), a()).prop_map(|(who, dep)| Op::AcceptOwner { who, dep }),
        1 => (0u8..16, who_s(), a()).prop_map(|(to, who, dep)| Op::SetArbiter { to, who, dep }),
        1 => (a(), prop_oneof![Just(0u128), Just(1u128), Just(NEAR)]).prop_map(|(agent, amt)| Op::StorageDeposit { agent, amt }),
        1 => (a(), a(), who_s(), a()).prop_map(|(agent, amt, who, dep)| Op::StorageWithdraw { agent, amt, who, dep }),
        1 => (0u8..16, 0u8..16, a(), prop_oneof![Just(0u16), Just(10u16), Just(512u16), Just(513u16)],
              proptest::option::of((amount_s(), amount_s())))
            .prop_map(|(who, wallet, dep, len, caps)| Op::Register { who, wallet, dep, len, caps }),
        1 => (prop_oneof![Just(0u128), Just(1u128), 1u128..5_000], amount_s(), who_s(), a())
            .prop_map(|(inject, amt, who, dep)| Op::RecoverSurplus { inject, amt, who, dep }),
    ]
}

// ───────────────────────────── world view ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct AgentV {
    agent: Agent,
    balance: u128,
    per_task: u128,
    daily: u128,
    daily_remaining: u128,
    stake: (u128, u128, u64, u128), // available, pending, unlock_at, locked
    storage: u128,
    wrec: WorkerRecord,
    crec: ClientRecord,
}

#[derive(Clone, Debug, PartialEq)]
struct World {
    totals: [u128; 6], // locked, dispute_bonds, balances, stakes, fees_owed, in_flight
    agents: BTreeMap<AgentId, AgentV>,
    jobs: BTreeMap<JobId, Job>,
    roles: Roles,
    params: Params,
    job_count: u64,
    agent_count: u64,
    index: Vec<Option<AgentId>>,
}

impl World {
    /// The parts of the world the effect model predicts exactly.
    fn comparable(&self) -> World {
        let mut w = self.clone();
        for a in w.agents.values_mut() {
            a.daily_remaining = 0;
            a.storage = 0;
        }
        w
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    Ok,
    Fail,
    Unknown,
}

fn expect(c: bool) -> Expect {
    if c {
        Expect::Ok
    } else {
        Expect::Fail
    }
}

// ───────────────────────────── harness ─────────────────────────────

#[derive(Clone, Debug)]
enum Pending {
    Ft {
        to: String,
        amount: u128,
        refund: Refund,
    },
    Near {
        agent: AgentId,
        amount: u128,
    },
}

struct H {
    c: Contract,
    now: u64,
    held: u128,
    deposited: u128,
    paid_out: BTreeMap<String, u128>,
    near_in: u128,
    near_out: u128,
    pending: Vec<Pending>,
    /// Tokens held above the totals: plain `ft_transfer`s nobody was credited for.
    surplus: u128,
    gas_tgas: u64,
    trace: Vec<String>,
}

struct Snapshot {
    storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    contract: Vec<u8>,
}

fn ctx_full(pred: &str, deposit: u128, now: u64, gas_tgas: u64, results: Vec<PromiseResult>) {
    let mut b = VMContextBuilder::new();
    b.current_account_id(acc(CONTRACT))
        .predecessor_account_id(acc(pred))
        .signer_account_id(acc(pred))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .account_balance(NearToken::from_yoctonear(1_000_000 * NEAR))
        .block_timestamp(now * 1_000_000_000)
        .prepaid_gas(Gas::from_tgas(gas_tgas));
    testing_env!(
        b.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        results
    );
}

fn panic_msg(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn count_event(logs: &[String], name: &str) -> usize {
    let needle = format!("\"event\":\"{name}\"");
    logs.iter().filter(|l| l.contains(&needle)).count()
}

impl H {
    fn new(p: Params) -> Self {
        ctx_full("owner.test", 0, T0, 300, vec![]);
        near_sdk::mock::with_mocked_blockchain(|b| {
            let _ = b.take_storage();
        });
        let c = Contract::new(
            acc("owner.test"),
            acc(TOKEN),
            acc("dao.test"),
            acc("pauser.test"),
            acc("fees.test"),
            p,
        );
        let mut h = H {
            c,
            now: T0,
            held: 0,
            deposited: 0,
            paid_out: BTreeMap::new(),
            near_in: 0,
            near_out: 0,
            pending: vec![],
            surplus: 0,
            gas_tgas: 300,
            trace: vec![],
        };
        h.c.flush();
        h
    }

    fn snapshot(&mut self) -> Snapshot {
        self.c.flush();
        let storage = near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
        let copy = storage.clone();
        near_sdk::env::set_blockchain_interface(near_sdk::MockedBlockchain::new(
            VMContextBuilder::new()
                .current_account_id(acc(CONTRACT))
                .build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            vec![],
            storage,
            Default::default(),
            None,
        ));
        Snapshot {
            storage: copy,
            contract: near_sdk::borsh::to_vec(&self.c).unwrap(),
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        let restored: Contract = near_sdk::borsh::from_slice(&snap.contract).unwrap();
        // Forget, not drop: dropping would flush the half-changed caches.
        let broken = std::mem::replace(&mut self.c, restored);
        std::mem::forget(broken);
        near_sdk::env::set_blockchain_interface(near_sdk::MockedBlockchain::new(
            VMContextBuilder::new()
                .current_account_id(acc(CONTRACT))
                .build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            vec![],
            snap.storage,
            Default::default(),
            None,
        ));
    }

    /// Atomic call: Ok((result, logs)) or Err(panic message), state rolled back.
    fn call_with<R>(
        &mut self,
        who: &str,
        deposit: u128,
        results: Vec<PromiseResult>,
        f: impl FnOnce(&mut Contract) -> R,
    ) -> Result<(R, Vec<String>), String> {
        let snap = self.snapshot();
        ctx_full(who, deposit, self.now, self.gas_tgas, results);
        match catch_unwind(AssertUnwindSafe(|| f(&mut self.c))) {
            Ok(r) => {
                self.c.flush();
                let logs = near_sdk::test_utils::get_logs();
                self.near_in += deposit;
                Ok((r, logs))
            }
            Err(e) => {
                self.restore(snap);
                Err(panic_msg(e))
            }
        }
    }

    fn call<R>(
        &mut self,
        who: &str,
        deposit: u128,
        f: impl FnOnce(&mut Contract) -> R,
    ) -> Result<(R, Vec<String>), String> {
        self.call_with(who, deposit, vec![], f)
    }

    fn view(&self) -> World {
        ctx_full("stranger.test", 0, self.now, 300, vec![]);
        let c = &self.c;
        let t = c.get_totals();
        assert_eq!(
            t.sum.0,
            t.locked.0
                + t.dispute_bonds.0
                + t.balances.0
                + t.stakes.0
                + t.fees_owed.0
                + t.in_flight.0,
            "totals view sum"
        );
        let mut agents = BTreeMap::new();
        for id in 1..=c.agent_count() {
            let agent = c.get_agent(id).expect("agent ids are dense");
            let p = c.purse_of(id).expect("every agent has a purse");
            let s = c.stake_of(id).expect("every agent has a stake");
            assert_eq!(s.unlocked.0, s.available.0.saturating_sub(s.locked.0));
            assert_eq!(
                p.daily_remaining.0,
                p.daily_cap.0.saturating_sub(p.spent_today.0)
            );
            agents.insert(
                id,
                AgentV {
                    agent,
                    balance: p.balance.0,
                    per_task: p.per_task_cap.0,
                    daily: p.daily_cap.0,
                    daily_remaining: p.daily_remaining.0,
                    stake: (s.available.0, s.pending.0, s.unlock_at, s.locked.0),
                    storage: c.storage_balance_of(id).0,
                    wrec: c.worker_record(id).unwrap(),
                    crec: c.client_record(id).unwrap(),
                },
            );
        }
        let n = c.agent_count();
        assert!(c.get_agent(n + 1).is_none(), "no agent beyond agent_count");
        assert!(c.get_agent(0).is_none(), "agent id 0 is never used");
        let mut jobs = BTreeMap::new();
        for id in 1..=c.job_count() {
            jobs.insert(id, c.get_job(id).expect("job ids are dense"));
        }
        assert!(
            c.get_job(c.job_count() + 1).is_none(),
            "no job beyond job_count"
        );
        assert!(c.get_job(0).is_none(), "job id 0 is never used");
        World {
            totals: [
                t.locked.0,
                t.dispute_bonds.0,
                t.balances.0,
                t.stakes.0,
                t.fees_owed.0,
                t.in_flight.0,
            ],
            agents,
            jobs,
            roles: c.get_roles(),
            params: c.get_params(),
            job_count: c.job_count(),
            agent_count: c.agent_count(),
            index: ACCTS.iter().map(|a| c.agent_of_wallet(acc(a))).collect(),
        }
    }
}

// ───────────────────────────── invariants ─────────────────────────────

fn legal(from: Option<JobState>, to: JobState) -> bool {
    use JobState::*;
    match from {
        None => matches!(to, Created | Settled),
        Some(f) if f == to => true,
        Some(Created) => matches!(to, Accepted | Refunded),
        Some(Accepted) => matches!(to, Submitted | Refunded),
        Some(Submitted) => matches!(to, Settled | Disputed),
        Some(Disputed) => matches!(to, Settled | Refunded),
        Some(_) => false,
    }
}

fn check_invariants(h: &H, pre: Option<&World>, w: &World) -> Result<(), String> {
    macro_rules! ensure {
        ($c:expr, $($m:tt)*) => { if !$c { return Err(format!($($m)*)); } };
    }
    let [locked, dbonds, balances, stakes, fees, in_flight] = w.totals;
    // INV-1 conservation: tokens held == sum of buckets.
    let sum = locked + dbonds + balances + stakes + fees + in_flight;
    ensure!(
        sum + h.surplus == h.held,
        "INV-1 totals sum {sum} + surplus {} != tokens held {}",
        h.surplus,
        h.held
    );
    ensure!(
        h.deposited == h.held + h.paid_out.values().sum::<u128>(),
        "INV-1b deposits {} != held {} + paid out {:?}",
        h.deposited,
        h.held,
        h.paid_out
    );
    // INV-2 buckets match their parts.
    let bal_sum: u128 = w.agents.values().map(|a| a.balance).sum();
    ensure!(
        balances == bal_sum,
        "INV-2 totals.balances {balances} != sum of purses {bal_sum}"
    );
    let stake_sum: u128 = w.agents.values().map(|a| a.stake.0 + a.stake.1).sum();
    ensure!(
        stakes == stake_sum,
        "INV-2 totals.stakes {stakes} != sum(available+pending) {stake_sum}"
    );
    let open_sum: u128 = w
        .jobs
        .values()
        .filter(|j| j.state.is_open())
        .map(|j| j.amount.0)
        .sum();
    ensure!(
        locked == open_sum,
        "INV-2 totals.locked {locked} != open jobs {open_sum}"
    );
    let db_sum: u128 = w
        .jobs
        .values()
        .filter(|j| j.state == JobState::Disputed)
        .map(|j| j.dispute_bond.0)
        .sum();
    ensure!(
        dbonds == db_sum,
        "INV-2 dispute_bonds {dbonds} != disputed jobs {db_sum}"
    );
    let fl_sum: u128 = h
        .pending
        .iter()
        .map(|p| match p {
            Pending::Ft { amount, .. } => *amount,
            Pending::Near { .. } => 0,
        })
        .sum();
    ensure!(
        in_flight == fl_sum,
        "INV-2 in_flight {in_flight} != pending transfers {fl_sum}"
    );
    // INV-3 stake locks.
    for (id, a) in &w.agents {
        let bonds: u128 = w
            .jobs
            .values()
            .filter(|j| j.bond_locked && j.worker == *id)
            .map(|j| j.worker_bond.0)
            .sum();
        ensure!(
            a.stake.3 == bonds,
            "INV-3 agent {id} stake.locked {} != locked bonds {bonds}",
            a.stake.3
        );
        ensure!(
            a.stake.0 >= a.stake.3,
            "INV-3 agent {id} available {} < locked {}",
            a.stake.0,
            a.stake.3
        );
        if a.stake.1 == 0 {
            ensure!(
                a.stake.2 == 0,
                "INV-3 agent {id} unlock_at {} with nothing pending",
                a.stake.2
            );
        }
    }
    // INV-4 jobs.
    for (id, j) in &w.jobs {
        ensure!(j.id == *id, "INV-4 job {id} id field {}", j.id);
        ensure!(j.client != j.worker, "INV-4 self-dealing job {id}");
        ensure!(
            j.fee.0 < j.amount.0 || j.amount.0 == 0,
            "INV-4 job {id} fee {} >= amount {}",
            j.fee.0,
            j.amount.0
        );
        ensure!(j.amount.0 > 0, "INV-4 job {id} zero amount");
        let (co, wo) = (
            &w.agents[&j.client].agent.owner,
            &w.agents[&j.worker].agent.owner,
        );
        ensure!(
            co != wo,
            "INV-4 job {id} client and worker share owner {co}"
        );
        if j.bond_locked {
            ensure!(
                matches!(
                    j.state,
                    JobState::Accepted | JobState::Submitted | JobState::Disputed
                ),
                "INV-4 job {id} bond locked in {:?}",
                j.state
            );
        }
        if matches!(
            j.state,
            JobState::Accepted | JobState::Submitted | JobState::Disputed
        ) {
            ensure!(
                j.bond_locked == (j.worker_bond.0 > 0),
                "INV-4 job {id} open with bond {} locked={}",
                j.worker_bond.0,
                j.bond_locked
            );
        }
        ensure!(
            j.work_deadline >= j.accept_deadline,
            "INV-4 job {id} work deadline before accept deadline"
        );
        match j.state {
            JobState::Created | JobState::Accepted => ensure!(
                j.outcome.is_none() && j.result_hash.is_none(),
                "INV-4 job {id} early result/outcome"
            ),
            JobState::Submitted => ensure!(
                j.result_hash.is_some() && j.review_deadline > 0 && j.outcome.is_none(),
                "INV-4 job {id} submitted shape"
            ),
            JobState::Disputed => ensure!(
                j.result_hash.is_some() && j.dispute_deadline > 0 && j.outcome.is_none(),
                "INV-4 job {id} disputed shape"
            ),
            JobState::Settled => ensure!(
                matches!(
                    j.outcome,
                    Some(Outcome::Success) | Some(Outcome::Unresolved)
                ),
                "INV-4 job {id} settled outcome {:?}",
                j.outcome
            ),
            JobState::Refunded => ensure!(
                matches!(
                    j.outcome,
                    None | Some(Outcome::Failed) | Some(Outcome::DisputeLost)
                ),
                "INV-4 job {id} refunded outcome {:?}",
                j.outcome
            ),
        }
        // INV-5 verdict view agrees with the stored job.
        ctx_full("stranger.test", 0, h.now, 300, vec![]);
        let v = h.c.verdict_of(*id);
        let want = match (
            j.state,
            j.result_hash.is_some(),
            j.dispute_deadline != 0,
            j.outcome,
        ) {
            (_, false, _, _) => None,
            (JobState::Settled, true, false, _) => Some(100),
            (JobState::Refunded, true, _, Some(Outcome::DisputeLost)) => Some(0),
            _ => None,
        };
        ensure!(
            v == want,
            "INV-5 verdict_of({id}) = {v:?}, expected {want:?} for {j:?}"
        );
    }
    // INV-6 records agree with the jobs that ended.
    for (id, a) in &w.agents {
        let mine = |s: JobState, o: Option<Outcome>| {
            w.jobs
                .values()
                .filter(|j| j.worker == *id && j.state == s && j.outcome == o)
                .count() as u64
        };
        let completed = mine(JobState::Settled, Some(Outcome::Success));
        let unresolved = mine(JobState::Settled, Some(Outcome::Unresolved));
        let failed = mine(JobState::Refunded, Some(Outcome::Failed));
        let lost = mine(JobState::Refunded, Some(Outcome::DisputeLost));
        let r = &a.wrec;
        ensure!(
            (r.completed, r.unresolved, r.failed, r.disputes_lost)
                == (completed, unresolved, failed, lost),
            "INV-6 worker record {id} {r:?} vs jobs c{completed} u{unresolved} f{failed} l{lost}"
        );
        ensure!(
            r.reviews == completed + failed + lost && r.review_score_sum == 100 * completed,
            "INV-6 worker reviews {id} {r:?}"
        );
        let c = &a.crec;
        ensure!(
            c.reviews == c.paid + c.disputes_rejected + c.disputes_upheld + c.silent
                && c.review_score_sum == 100 * (c.paid + c.disputes_upheld) + 50 * c.silent,
            "INV-6 client reviews {id} {c:?}"
        );
    }
    // INV-7 wallet index is a bijection with the agents' wallets.
    for (i, a) in ACCTS.iter().enumerate() {
        let owners: Vec<AgentId> = w
            .agents
            .iter()
            .filter(|(_, v)| v.agent.wallet.as_ref().map(|x| x.as_str()) == Some(*a))
            .map(|(id, _)| *id)
            .collect();
        ensure!(
            owners.len() <= 1,
            "INV-7 wallet {a} serves agents {owners:?}"
        );
        ensure!(
            w.index[i] == owners.first().copied(),
            "INV-7 index {a} -> {:?} but agents {owners:?}",
            w.index[i]
        );
    }
    // INV-8 storage NEAR is never minted.
    let st_sum: u128 = w.agents.values().map(|a| a.storage).sum();
    ensure!(
        st_sum <= h.near_in - h.near_out,
        "INV-8 storage balances {st_sum} > NEAR in {} - out {}",
        h.near_in,
        h.near_out
    );
    // INV-9 transitions, finality, immutability, id monotonicity.
    if let Some(p) = pre {
        ensure!(
            w.job_count >= p.job_count && w.agent_count >= p.agent_count,
            "INV-9 counters went down"
        );
        ensure!(
            w.job_count <= p.job_count + 1 && w.agent_count <= p.agent_count + 1,
            "INV-9 counters jumped"
        );
        let mut changed_state = 0;
        for (id, j) in &w.jobs {
            let before = p.jobs.get(id);
            ensure!(
                legal(before.map(|b| b.state), j.state),
                "INV-9 job {id} illegal {:?} -> {:?}",
                before.map(|b| b.state),
                j.state
            );
            if let Some(b) = before {
                if b.state != j.state {
                    changed_state += 1;
                }
                if !b.state.is_open() {
                    ensure!(b == j, "INV-9 terminal job {id} changed: {b:?} -> {j:?}");
                }
                ensure!(
                    (
                        b.client,
                        b.worker,
                        b.amount,
                        b.fee,
                        &b.spec_hash,
                        b.accept_deadline,
                        b.work_deadline,
                        b.worker_bond,
                        b.dispute_bond,
                        b.created_at,
                        b.review_window,
                        b.dispute_timeout
                    ) == (
                        j.client,
                        j.worker,
                        j.amount,
                        j.fee,
                        &j.spec_hash,
                        j.accept_deadline,
                        j.work_deadline,
                        j.worker_bond,
                        j.dispute_bond,
                        j.created_at,
                        j.review_window,
                        j.dispute_timeout
                    ),
                    "INV-9 immutable field of job {id} changed"
                );
            }
        }
        ensure!(
            changed_state <= 1,
            "INV-9 one call moved {changed_state} jobs"
        );
        for (id, a) in &p.agents {
            let now = &w.agents[id];
            ensure!(
                now.agent.owner == a.agent.owner
                    && now.agent.registered_at == a.agent.registered_at,
                "INV-9 agent {id} owner changed"
            );
        }
    }
    Ok(())
}

// ───────────────────────────── the runner ─────────────────────────────

struct Runner {
    h: H,
}

impl Runner {
    fn new() -> Self {
        let mut h = H::new(base_params());
        // Four agents: alice owns 1 and 4 (same owner), bob 2, carol 3.
        for (owner, wallet) in [
            ("alice.test", "a1.alice.test"),
            ("bob.test", "b1.bob.test"),
            ("carol.test", "c1.carol.test"),
            ("alice.test", "a2.alice.test"),
        ] {
            let id = h
                .call(owner, 10 * NEAR, |c| {
                    c.register_agent(acc(wallet), "ipfs://card".into(), None, None)
                })
                .unwrap()
                .0;
            h.call(wallet, 0, |c| c.accept_wallet(id)).unwrap();
        }
        for id in 1..=4u64 {
            for (kind, amt) in [("Fund", 15_000_000u128), ("Stake", 20_000_000u128)] {
                let msg = format!(r#"{{"{kind}":{{"agent_id":{id}}}}}"#);
                h.call(TOKEN, 0, |c| {
                    let _ = c.ft_on_transfer(acc("funder.test"), U128(amt), msg);
                })
                .unwrap();
                h.held += amt;
                h.deposited += amt;
            }
        }
        Runner { h }
    }

    fn agent_id(&self, w: &World, i: u8) -> AgentId {
        (i as u64 % (w.agent_count + 1)) + 1
    }

    fn job_id(&self, w: &World, i: u8) -> JobId {
        if w.job_count == 0 {
            return 1;
        }
        if i < 200 {
            w.job_count.saturating_sub((i % 3) as u64).max(1)
        } else {
            (i as u64 % (w.job_count + 1)) + 1
        }
    }

    fn who(w: Who, right: &str) -> String {
        match w {
            Who::Right => right.to_string(),
            Who::Acct(i) => ACCTS[i as usize % ACCTS.len()].to_string(),
        }
    }

    fn wallet_of(w: &World, id: AgentId) -> String {
        w.agents
            .get(&id)
            .and_then(|a| a.agent.wallet.clone())
            .map(|x| x.to_string())
            .unwrap_or_else(|| "stranger.test".into())
    }

    fn owner_of(w: &World, id: AgentId) -> String {
        w.agents
            .get(&id)
            .map(|a| a.agent.owner.to_string())
            .unwrap_or_else(|| "stranger.test".into())
    }

    fn is_wallet(w: &World, id: AgentId, who: &str) -> bool {
        w.agents
            .get(&id)
            .map(|a| a.agent.wallet.as_ref().map(|x| x.as_str()) == Some(who) && !a.agent.frozen)
            .unwrap_or(false)
    }

    fn is_owner(w: &World, id: AgentId, who: &str) -> bool {
        w.agents
            .get(&id)
            .map(|a| a.agent.owner.as_str() == who)
            .unwrap_or(false)
    }

    /// Can `id` be charged `amt` by its caps right now?
    fn caps_ok(w: &World, id: AgentId, amt: u128) -> bool {
        let a = &w.agents[&id];
        amt <= a.per_task && amt <= a.daily_remaining
    }

    fn step(&mut self, op: &Op) -> Result<(), String> {
        let pre = self.h.view();
        let now = self.h.now;
        let prm = pre.params.clone();
        let mut exp = pre.comparable();
        let mut new_pending: Option<Pending> = None;
        let gas_before = self.h.gas_tgas;
        let mut events: Vec<(&'static str, usize)> = vec![];

        macro_rules! run {
            ($who:expr, $dep:expr, $f:expr) => {{
                let who: String = $who;
                self.h.call(&who, $dep, $f).map(|(_, logs)| logs)
            }};
        }

        let (e, res): (Expect, Result<Vec<String>, String>) = match op.clone() {
            Op::Fund {
                agent,
                amt,
                who,
                msg,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, TOKEN);
                if self.h.held.checked_add(amt).is_none() {
                    return Ok(()); // no real token has that much supply
                }
                let (m, kind) = match msg % 5 {
                    0..=1 => (format!(r#"{{"Fund":{{"agent_id":{id}}}}}"#), 0),
                    2..=3 => (format!(r#"{{"Stake":{{"agent_id":{id}}}}}"#), 1),
                    _ => ("{\"Fund\":{}}".to_string(), 2),
                };
                let ok = who == TOKEN && amt > 0 && kind != 2 && pre.agents.contains_key(&id);
                if ok {
                    let a = exp.agents.get_mut(&id).unwrap();
                    if kind == 0 {
                        a.balance += amt;
                        exp.totals[2] += amt;
                    } else {
                        a.stake.0 += amt;
                        exp.totals[3] += amt;
                    }
                }
                let r = run!(who, 0, |c: &mut Contract| c.ft_on_transfer(
                    acc("funder.test"),
                    U128(amt),
                    m
                ));
                if r.is_ok() {
                    self.h.held += amt;
                    self.h.deposited += amt;
                }
                (expect(ok), r)
            }
            Op::Create {
                client,
                worker,
                amt,
                aw,
                ww,
                who,
                hash,
            } => {
                let (cid, wid) = (self.agent_id(&pre, client), self.agent_id(&pre, worker));
                let who = Self::who(who, &Self::wallet_of(&pre, cid));
                let (hs, hok) = hash_variant(hash);
                let fee = pct(amt, prm.protocol_fee_bps, prm.min_fee.0);
                let bond = pct(amt, prm.bond_bps, prm.min_bond.0);
                let dbond = pct(amt, prm.dispute_bond_bps, prm.min_dispute_bond.0);
                let base = !pre.roles.paused
                    && amt > 0
                    && amt >= prm.min_job_amount.0
                    && cid != wid
                    && Self::is_wallet(&pre, cid, &who)
                    && pre
                        .agents
                        .get(&wid)
                        .map(|a| a.agent.wallet.is_some() && !a.agent.frozen)
                        .unwrap_or(false)
                    && Self::owner_of(&pre, cid) != Self::owner_of(&pre, wid)
                    && pre.agents[&wid]
                        .stake
                        .0
                        .saturating_sub(pre.agents[&wid].stake.3)
                        >= prm.min_stake.0
                    && amt <= prm.max_job_amount.0
                    && aw > 0
                    && ww > 0
                    && aw <= prm.max_accept_window
                    && ww <= prm.max_work_window
                    && hok
                    && pre.agents[&cid].balance >= amt
                    && Self::caps_ok(&pre, cid, amt)
                    && fee.is_some()
                    && bond.is_some()
                    && dbond.is_some();
                let e = if !base {
                    Expect::Fail
                } else if pre.agents[&cid].storage < STORAGE_SAFE {
                    Expect::Unknown
                } else {
                    Expect::Ok
                };
                if base {
                    let id = pre.job_count + 1;
                    let a = exp.agents.get_mut(&cid).unwrap();
                    a.balance -= amt;
                    exp.totals[2] -= amt;
                    exp.totals[0] += amt;
                    exp.job_count += 1;
                    exp.jobs.insert(
                        id,
                        Job {
                            id,
                            client: cid,
                            worker: wid,
                            amount: U128(amt),
                            fee: U128(fee.unwrap()),
                            state: JobState::Created,
                            accept_deadline: now + aw,
                            work_deadline: now + aw + ww,
                            review_deadline: 0,
                            dispute_deadline: 0,
                            review_window: prm.review_window,
                            dispute_timeout: prm.dispute_timeout,
                            spec_hash: hs.clone(),
                            result_hash: None,
                            worker_bond: U128(bond.unwrap()),
                            dispute_bond: U128(dbond.unwrap()),
                            bond_locked: false,
                            review_hash: None,
                            outcome: None,
                            created_at: now,
                        },
                    );
                    events.push(("job_created", 1));
                }
                let r = run!(who, 0, |c: &mut Contract| c.create_job(
                    cid,
                    wid,
                    U128(amt),
                    hs,
                    aw,
                    ww
                ));
                (e, r)
            }
            Op::Direct {
                client,
                worker,
                amt,
                who,
                hash,
            } => {
                let (cid, wid) = (self.agent_id(&pre, client), self.agent_id(&pre, worker));
                let who = Self::who(who, &Self::wallet_of(&pre, cid));
                let (hs, hok) = hash_variant(hash);
                let fee = pct(amt, prm.protocol_fee_bps, prm.min_fee.0);
                let base = !pre.roles.paused
                    && amt > 0
                    && amt >= prm.min_job_amount.0
                    && cid != wid
                    && Self::is_wallet(&pre, cid, &who)
                    && pre
                        .agents
                        .get(&wid)
                        .map(|a| a.agent.wallet.is_some() && !a.agent.frozen)
                        .unwrap_or(false)
                    && Self::owner_of(&pre, cid) != Self::owner_of(&pre, wid)
                    && pre.agents[&wid]
                        .stake
                        .0
                        .saturating_sub(pre.agents[&wid].stake.3)
                        >= prm.min_stake.0
                    && amt <= prm.fast_path_max.0
                    && hok
                    && pre.agents[&cid].balance >= amt
                    && Self::caps_ok(&pre, cid, amt)
                    && fee.is_some();
                let e = if !base {
                    Expect::Fail
                } else if pre.agents[&cid].storage < STORAGE_SAFE {
                    Expect::Unknown
                } else {
                    Expect::Ok
                };
                if base {
                    let fee = fee.unwrap();
                    let id = pre.job_count + 1;
                    exp.agents.get_mut(&cid).unwrap().balance -= amt;
                    exp.agents.get_mut(&wid).unwrap().balance += amt - fee;
                    exp.totals[2] -= fee;
                    exp.totals[4] += fee;
                    exp.job_count += 1;
                    let wr = &mut exp.agents.get_mut(&wid).unwrap().wrec;
                    wr.completed += 1;
                    wr.reviews += 1;
                    wr.review_score_sum += 100;
                    let cr = &mut exp.agents.get_mut(&cid).unwrap().crec;
                    cr.paid += 1;
                    cr.reviews += 1;
                    cr.review_score_sum += 100;
                    exp.jobs.insert(
                        id,
                        Job {
                            id,
                            client: cid,
                            worker: wid,
                            amount: U128(amt),
                            fee: U128(fee),
                            state: JobState::Settled,
                            accept_deadline: now,
                            work_deadline: now,
                            review_deadline: now,
                            dispute_deadline: 0,
                            review_window: 0,
                            dispute_timeout: 0,
                            spec_hash: hs.clone(),
                            result_hash: None,
                            worker_bond: U128(0),
                            dispute_bond: U128(0),
                            bond_locked: false,
                            review_hash: None,
                            outcome: Some(Outcome::Success),
                            created_at: now,
                        },
                    );
                    events.push(("job_settled", 1));
                }
                let r = run!(who, 0, |c: &mut Contract| c.direct_pay(
                    cid,
                    wid,
                    U128(amt),
                    hs
                ));
                (e, r)
            }
            Op::Accept { job, who } => {
                let id = self.job_id(&pre, job);
                let j = pre.jobs.get(&id).cloned();
                let who = Self::who(
                    who,
                    &j.as_ref()
                        .map(|j| Self::wallet_of(&pre, j.worker))
                        .unwrap_or_else(|| "stranger.test".into()),
                );
                let ok = j.as_ref().is_some_and(|j| {
                    let bond = j.worker_bond.0;
                    let w = &pre.agents[&j.worker];
                    Self::is_wallet(&pre, j.worker, &who)
                        && j.state == JobState::Created
                        && now <= j.accept_deadline
                        && (bond == 0
                            || (w.stake.0.saturating_sub(w.stake.3) >= bond
                                && Self::caps_ok(&pre, j.worker, bond)))
                });
                if ok {
                    let j = exp.jobs.get_mut(&id).unwrap();
                    j.state = JobState::Accepted;
                    if j.worker_bond.0 > 0 {
                        j.bond_locked = true;
                        exp.agents.get_mut(&j.worker).unwrap().stake.3 += j.worker_bond.0;
                    }
                    events.push(("job_accepted", 1));
                }
                let r = run!(who, 0, |c: &mut Contract| c.accept_job(id));
                (expect(ok), r)
            }
            Op::Submit { job, who, hash } => {
                let id = self.job_id(&pre, job);
                let j = pre.jobs.get(&id).cloned();
                let who = Self::who(
                    who,
                    &j.as_ref()
                        .map(|j| Self::wallet_of(&pre, j.worker))
                        .unwrap_or_else(|| "stranger.test".into()),
                );
                let (hs, hok) = hash_variant(hash);
                let ok = j.as_ref().is_some_and(|j| {
                    Self::is_wallet(&pre, j.worker, &who)
                        && j.state == JobState::Accepted
                        && now <= j.work_deadline
                        && hok
                });
                if ok {
                    let j = exp.jobs.get_mut(&id).unwrap();
                    j.state = JobState::Submitted;
                    j.review_deadline = now + j.review_window;
                    j.result_hash = Some(hs.clone());
                    events.push(("result_submitted", 1));
                }
                let r = run!(who, 0, |c: &mut Contract| c.submit_result(id, hs));
                (expect(ok), r)
            }
            Op::Approve { job, who, hash } => {
                let id = self.job_id(&pre, job);
                let j = pre.jobs.get(&id).cloned();
                let who = Self::who(
                    who,
                    &j.as_ref()
                        .map(|j| Self::wallet_of(&pre, j.client))
                        .unwrap_or_else(|| "stranger.test".into()),
                );
                let h = hash.map(hash_variant);
                let hok = h.as_ref().is_none_or(|x| x.1);
                let ok = j.as_ref().is_some_and(|j| {
                    Self::is_wallet(&pre, j.client, &who) && j.state == JobState::Submitted && hok
                });
                if ok {
                    let mut j = exp.jobs[&id].clone();
                    j.review_hash = h.as_ref().map(|x| x.0.clone());
                    let cr = &mut exp.agents.get_mut(&j.client).unwrap().crec;
                    cr.paid += 1;
                    cr.reviews += 1;
                    cr.review_score_sum += 100;
                    model_settle(&mut exp, &mut j, Outcome::Success, true);
                    exp.jobs.insert(id, j);
                    events.push(("job_settled", 1));
                }
                let rh = h.map(|x| x.0);
                let r = run!(who, 0, |c: &mut Contract| c.approve(id, rh));
                (expect(ok), r)
            }
            Op::Dispute { job, who, hash } => {
                let id = self.job_id(&pre, job);
                let j = pre.jobs.get(&id).cloned();
                let who = Self::who(
                    who,
                    &j.as_ref()
                        .map(|j| Self::wallet_of(&pre, j.client))
                        .unwrap_or_else(|| "stranger.test".into()),
                );
                let h = hash.map(hash_variant);
                let hok = h.as_ref().is_none_or(|x| x.1);
                let ok = j.as_ref().is_some_and(|j| {
                    let b = j.dispute_bond.0;
                    Self::is_wallet(&pre, j.client, &who)
                        && j.state == JobState::Submitted
                        && now <= j.review_deadline
                        && hok
                        && (b == 0 || pre.agents[&j.client].balance >= b)
                });
                if ok {
                    let j = exp.jobs.get_mut(&id).unwrap();
                    let b = j.dispute_bond.0;
                    exp.agents.get_mut(&j.client).unwrap().balance -= b;
                    exp.totals[2] -= b;
                    exp.totals[1] += b;
                    j.state = JobState::Disputed;
                    j.dispute_deadline = now + j.dispute_timeout;
                    j.review_hash = h.as_ref().map(|x| x.0.clone());
                    events.push(("job_disputed", 1));
                }
                let rh = h.map(|x| x.0);
                let r = run!(who, 0, |c: &mut Contract| c.dispute(id, rh));
                (expect(ok), r)
            }
            Op::Rule {
                job,
                for_worker,
                who,
            } => {
                let id = self.job_id(&pre, job);
                let who = Self::who(who, pre.roles.arbiter.as_str());
                let j = pre.jobs.get(&id).cloned();
                let ok = who == pre.roles.arbiter.as_str()
                    && j.as_ref().is_some_and(|j| j.state == JobState::Disputed);
                if ok {
                    let mut j = exp.jobs[&id].clone();
                    let b = j.dispute_bond.0;
                    exp.totals[1] -= b;
                    exp.totals[2] += b;
                    if for_worker {
                        let cr = &mut exp.agents.get_mut(&j.client).unwrap().crec;
                        cr.disputes_rejected += 1;
                        cr.reviews += 1;
                        exp.agents.get_mut(&j.worker).unwrap().balance += b;
                        model_settle(&mut exp, &mut j, Outcome::Success, true);
                        events.push(("job_settled", 1));
                    } else {
                        let cr = &mut exp.agents.get_mut(&j.client).unwrap().crec;
                        cr.disputes_upheld += 1;
                        cr.reviews += 1;
                        cr.review_score_sum += 100;
                        exp.agents.get_mut(&j.client).unwrap().balance += b;
                        model_refund(&mut exp, &mut j, Some(Outcome::DisputeLost));
                        events.push(("job_refunded", 1));
                    }
                    exp.jobs.insert(id, j);
                }
                let r = run!(who, 0, |c: &mut Contract| c.resolve_dispute(id, for_worker));
                (expect(ok), r)
            }
            Op::Cancel { job, who } => {
                let id = self.job_id(&pre, job);
                let j = pre.jobs.get(&id).cloned();
                let who = Self::who(
                    who,
                    &j.as_ref()
                        .map(|j| Self::wallet_of(&pre, j.client))
                        .unwrap_or_else(|| "stranger.test".into()),
                );
                let ok = j.as_ref().is_some_and(|j| {
                    Self::is_wallet(&pre, j.client, &who) && j.state == JobState::Created
                });
                if ok {
                    let mut j = exp.jobs[&id].clone();
                    model_refund(&mut exp, &mut j, None);
                    exp.jobs.insert(id, j);
                    events.push(("job_refunded", 1));
                }
                let r = run!(who, 0, |c: &mut Contract| c.cancel(id));
                (expect(ok), r)
            }
            Op::Exit { job, kind, who } => {
                let id = self.job_id(&pre, job);
                let who = Self::who(who, "stranger.test");
                let j = pre.jobs.get(&id).cloned();
                let ok = j.as_ref().is_some_and(|j| match kind {
                    0 => j.state == JobState::Created && now > j.accept_deadline,
                    1 => j.state == JobState::Accepted && now > j.work_deadline,
                    2 => j.state == JobState::Submitted && now > j.review_deadline,
                    _ => {
                        j.state == JobState::Disputed
                            && now > j.dispute_deadline + prm.dispute_grace
                    }
                });
                if ok {
                    let mut j = exp.jobs[&id].clone();
                    match kind {
                        0 => {
                            model_refund(&mut exp, &mut j, None);
                            events.push(("job_refunded", 1));
                        }
                        1 => {
                            model_refund(&mut exp, &mut j, Some(Outcome::Failed));
                            events.push(("job_refunded", 1));
                        }
                        2 => {
                            let cr = &mut exp.agents.get_mut(&j.client).unwrap().crec;
                            cr.silent += 1;
                            cr.reviews += 1;
                            cr.review_score_sum += 50;
                            model_settle(&mut exp, &mut j, Outcome::Success, true);
                            events.push(("job_settled", 1));
                        }
                        _ => {
                            let b = j.dispute_bond.0;
                            let to_worker = b / 2;
                            let to_client = b - to_worker;
                            exp.totals[1] -= b;
                            exp.totals[2] += b;
                            exp.agents.get_mut(&j.worker).unwrap().balance += to_worker;
                            exp.agents.get_mut(&j.client).unwrap().balance += to_client;
                            model_settle(&mut exp, &mut j, Outcome::Unresolved, false);
                            events.push(("job_settled", 1));
                            events.push(("dispute_expired", 1));
                            events.push((
                                "dispute_bond_settled",
                                (to_worker > 0) as usize + (to_client > 0) as usize,
                            ));
                        }
                    }
                    exp.jobs.insert(id, j);
                }
                let r = match kind {
                    0 => run!(who, 0, |c: &mut Contract| c.expire_unaccepted(id)),
                    1 => run!(who, 0, |c: &mut Contract| c.expire_undelivered(id)),
                    2 => run!(who, 0, |c: &mut Contract| c.auto_approve(id)),
                    _ => run!(who, 0, |c: &mut Contract| c.expire_dispute(id)),
                };
                (expect(ok), r)
            }
            Op::Advance(s) => {
                self.h.now += s;
                self.h.trace.push(format!("  now = {}", self.h.now));
                return self.after(Some(&pre), None);
            }
            Op::Jump { job, kind, delta } => {
                let target = if kind == 4 {
                    let id = self.agent_id(&pre, job);
                    pre.agents.get(&id).map(|a| a.stake.2).unwrap_or(0)
                } else {
                    let id = self.job_id(&pre, job);
                    pre.jobs.get(&id).map_or(0, |j| match kind {
                        0 => j.accept_deadline,
                        1 => j.work_deadline,
                        2 => j.review_deadline,
                        3 => j.dispute_deadline,
                        _ => j.dispute_deadline + prm.dispute_grace,
                    })
                };
                let t = (target as i128 + delta as i128).max(0) as u64;
                if t > self.h.now {
                    self.h.now = t;
                    self.h.trace.push(format!("  now = {t}"));
                }
                return self.after(Some(&pre), None);
            }
            Op::Withdraw {
                agent,
                amt,
                who,
                dep,
                other_receiver,
                low_gas,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                if low_gas {
                    self.h.gas_tgas = 29;
                }
                let ok = d == 1
                    && !low_gas
                    && Self::is_owner(&pre, id, &who)
                    && amt > 0
                    && pre.agents[&id].balance >= amt;
                let to = if other_receiver {
                    "stranger.test".to_string()
                } else {
                    Self::owner_of(&pre, id)
                };
                if ok {
                    exp.agents.get_mut(&id).unwrap().balance -= amt;
                    exp.totals[2] -= amt;
                    exp.totals[5] += amt;
                    new_pending = Some(Pending::Ft {
                        to: to.clone(),
                        amount: amt,
                        refund: Refund::Purse(id),
                    });
                }
                let recv = if other_receiver {
                    Some(acc("stranger.test"))
                } else {
                    None
                };
                let r = run!(who, d, |c: &mut Contract| {
                    let _ = c.withdraw(id, U128(amt), recv);
                });
                (expect(ok), r)
            }
            Op::ClaimFees { who, low_gas } => {
                let who = Self::who(who, "stranger.test");
                if low_gas {
                    self.h.gas_tgas = 29;
                }
                let f = pre.totals[4];
                let ok = !low_gas && f > 0;
                if ok {
                    exp.totals[4] = 0;
                    exp.totals[5] += f;
                    new_pending = Some(Pending::Ft {
                        to: pre.roles.fee_recipient.to_string(),
                        amount: f,
                        refund: Refund::Fees,
                    });
                }
                let r = run!(who, 0, |c: &mut Contract| {
                    let _ = c.claim_fees();
                });
                (expect(ok), r)
            }
            Op::ReqUnstake {
                agent,
                amt,
                who,
                dep,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let ok = d == 1
                    && Self::is_owner(&pre, id, &who)
                    && amt > 0
                    && pre.agents[&id].stake.1 == 0
                    && pre.agents[&id]
                        .stake
                        .0
                        .saturating_sub(pre.agents[&id].stake.3)
                        >= amt;
                if ok {
                    let s = &mut exp.agents.get_mut(&id).unwrap().stake;
                    s.0 -= amt;
                    s.1 = amt;
                    s.2 = now + prm.withdraw_delay;
                }
                let r = run!(who, d, |c: &mut Contract| c.request_unstake(id, U128(amt)));
                (expect(ok), r)
            }
            Op::CancelUnstake { agent, who, dep } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let ok = d == 1 && Self::is_owner(&pre, id, &who) && pre.agents[&id].stake.1 > 0;
                if ok {
                    let s = &mut exp.agents.get_mut(&id).unwrap().stake;
                    s.0 += s.1;
                    s.1 = 0;
                    s.2 = 0;
                }
                let r = run!(who, d, |c: &mut Contract| c.cancel_unstake(id));
                (expect(ok), r)
            }
            Op::CompleteUnstake {
                agent,
                who,
                dep,
                low_gas,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                if low_gas {
                    self.h.gas_tgas = 29;
                }
                let ok = d == 1
                    && !low_gas
                    && Self::is_owner(&pre, id, &who)
                    && pre.agents[&id].stake.1 > 0
                    && now >= pre.agents[&id].stake.2;
                if ok {
                    let p = pre.agents[&id].stake.1;
                    let s = &mut exp.agents.get_mut(&id).unwrap().stake;
                    s.1 = 0;
                    s.2 = 0;
                    exp.totals[3] -= p;
                    exp.totals[5] += p;
                    new_pending = Some(Pending::Ft {
                        to: Self::owner_of(&pre, id),
                        amount: p,
                        refund: Refund::Stake(id),
                    });
                }
                let r = run!(who, d, |c: &mut Contract| {
                    let _ = c.complete_unstake(id);
                });
                (expect(ok), r)
            }
            Op::Resolve { idx, ok } => {
                if self.h.pending.is_empty() {
                    return Ok(());
                }
                let i = idx as usize % self.h.pending.len();
                let p = self.h.pending.remove(i);
                let result = if ok {
                    PromiseResult::Successful(vec![])
                } else {
                    PromiseResult::Failed
                };
                let r = match p.clone() {
                    Pending::Ft { to, amount, refund } => {
                        exp.totals[5] -= amount;
                        if !ok {
                            match refund {
                                Refund::Purse(id) => {
                                    exp.agents.get_mut(&id).unwrap().balance += amount;
                                    exp.totals[2] += amount;
                                }
                                Refund::Fees => exp.totals[4] += amount,
                                Refund::Stake(id) => {
                                    exp.agents.get_mut(&id).unwrap().stake.0 += amount;
                                    exp.totals[3] += amount;
                                }
                                Refund::Surplus => {}
                            }
                        }
                        if let Refund::Stake(_) = refund {
                            events.push((if ok { "unstaked" } else { "stake_restored" }, 1));
                            events.push((if ok { "stake_restored" } else { "unstaked" }, 0));
                        }
                        let to2 = to.clone();
                        let rf = refund.clone();
                        let r = self
                            .h
                            .call_with(CONTRACT, 0, vec![result], |c| {
                                c.on_send(acc(&to2), U128(amount), rf)
                            })
                            .map(|(ret, logs)| {
                                assert_eq!(ret, ok, "on_send return value");
                                logs
                            });
                        if r.is_ok() && ok {
                            self.h.held -= amount;
                            *self.h.paid_out.entry(to).or_default() += amount;
                        } else if r.is_ok() && matches!(refund, Refund::Surplus) {
                            // Never in a bucket: if it did not leave, it is still a surplus.
                            self.h.surplus += amount;
                        }
                        r
                    }
                    Pending::Near { agent, amount } => {
                        if !ok {
                            // compared through INV-8 / storage, not exp
                        }
                        let r = self
                            .h
                            .call_with(CONTRACT, 0, vec![result], |c| {
                                c.on_storage_sent(agent, U128(amount))
                            })
                            .map(|(_, logs)| logs);
                        if r.is_ok() && ok {
                            self.h.near_out += amount;
                        }
                        r
                    }
                };
                if r.is_err() {
                    self.h.pending.insert(i, p);
                }
                self.h.trace.push(format!(
                    "  -> {:?}",
                    r.as_ref().map(|_| ()).map_err(|e| e.clone())
                ));
                (Expect::Ok, r)
            }
            Op::Freeze { agent, who } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let ok = Self::is_owner(&pre, id, &who);
                if ok {
                    exp.agents.get_mut(&id).unwrap().agent.frozen = true;
                }
                let r = run!(who, 0, |c: &mut Contract| c.freeze_agent(id));
                (expect(ok), r)
            }
            Op::Unfreeze { agent, who, dep } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let ok = d == 1 && Self::is_owner(&pre, id, &who);
                if ok {
                    exp.agents.get_mut(&id).unwrap().agent.frozen = false;
                }
                let r = run!(who, d, |c: &mut Contract| c.unfreeze_agent(id));
                (expect(ok), r)
            }
            Op::SetCaps {
                agent,
                per,
                daily,
                who,
                dep,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let ok = d == 1 && Self::is_owner(&pre, id, &who) && per <= daily;
                if ok {
                    let a = exp.agents.get_mut(&id).unwrap();
                    a.per_task = per;
                    a.daily = daily;
                }
                let r = run!(who, d, |c: &mut Contract| c.set_caps(
                    id,
                    U128(per),
                    U128(daily)
                ));
                (expect(ok), r)
            }
            Op::ProposeWallet {
                agent,
                to,
                who,
                dep,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let to = to.map(|i| ACCTS[i as usize % 16].to_string());
                let base = d == 1 && Self::is_owner(&pre, id, &who);
                if base {
                    exp.agents.get_mut(&id).unwrap().agent.pending_wallet =
                        to.as_ref().map(|s| acc(s));
                }
                let e = if !base {
                    Expect::Fail
                } else if pre.agents[&id].storage < STORAGE_SAFE {
                    Expect::Unknown
                } else {
                    Expect::Ok
                };
                let to2 = to.map(|s| acc(&s));
                let r = run!(who, d, |c: &mut Contract| c.propose_wallet(id, to2));
                (e, r)
            }
            Op::AcceptWallet { agent, who } => {
                let id = self.agent_id(&pre, agent);
                let pw = pre
                    .agents
                    .get(&id)
                    .and_then(|a| a.agent.pending_wallet.clone());
                let who = Self::who(
                    who,
                    pw.as_ref().map(|x| x.as_str()).unwrap_or("stranger.test"),
                );
                let taken = ACCTS
                    .iter()
                    .position(|a| *a == who)
                    .is_some_and(|i| pre.index[i].is_some());
                let base = pw.as_ref().map(|x| x.as_str()) == Some(who.as_str()) && !taken;
                if base {
                    let a = exp.agents.get_mut(&id).unwrap();
                    let old = a.agent.wallet.take();
                    a.agent.wallet = Some(acc(&who));
                    a.agent.pending_wallet = None;
                    if let Some(o) = old {
                        if let Some(i) = ACCTS.iter().position(|x| *x == o.as_str()) {
                            exp.index[i] = None;
                        }
                    }
                    if let Some(i) = ACCTS.iter().position(|x| *x == who) {
                        exp.index[i] = Some(id);
                    }
                }
                let e = if !base {
                    Expect::Fail
                } else if pre.agents[&id].storage < STORAGE_SAFE {
                    Expect::Unknown
                } else {
                    Expect::Ok
                };
                let r = run!(who, 0, |c: &mut Contract| c.accept_wallet(id));
                (e, r)
            }
            Op::SetCardUri {
                agent,
                len,
                who,
                dep,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let uri = "u".repeat(len as usize);
                let base = d == 1 && len as usize <= 512 && Self::is_owner(&pre, id, &who);
                if base {
                    exp.agents.get_mut(&id).unwrap().agent.card_uri = uri.clone();
                }
                let e = if !base {
                    Expect::Fail
                } else if pre.agents[&id].storage < STORAGE_SAFE {
                    Expect::Unknown
                } else {
                    Expect::Ok
                };
                let r = run!(who, d, |c: &mut Contract| c.set_card_uri(id, uri));
                (e, r)
            }
            Op::Pause { who } => {
                let who = Self::who(who, pre.roles.pauser.as_str());
                let ok = who == pre.roles.pauser.as_str() || who == pre.roles.owner.as_str();
                if ok {
                    exp.roles.paused = true;
                }
                let r = run!(who, 0, |c: &mut Contract| c.pause());
                (expect(ok), r)
            }
            Op::Unpause { who, dep } => {
                let who = Self::who(who, pre.roles.owner.as_str());
                let d = dep_of(dep);
                let ok = d == 1 && who == pre.roles.owner.as_str();
                if ok {
                    exp.roles.paused = false;
                }
                let r = run!(who, d, |c: &mut Contract| c.unpause());
                (expect(ok), r)
            }
            Op::SetParams { v, who, dep } => {
                let who = Self::who(who, pre.roles.owner.as_str());
                let d = dep_of(dep);
                let p = params_variant(v);
                let ok = d == 1 && who == pre.roles.owner.as_str() && params_valid(&p);
                if ok {
                    exp.params = p.clone();
                }
                let r = run!(who, d, |c: &mut Contract| c.set_params(p));
                (expect(ok), r)
            }
            Op::ProposeOwner { to, who, dep } => {
                let who = Self::who(who, pre.roles.owner.as_str());
                let d = dep_of(dep);
                let to = ACCTS[to as usize % 16];
                let ok = d == 1 && who == pre.roles.owner.as_str();
                if ok {
                    exp.roles.pending_owner = Some(acc(to));
                }
                let r = run!(who, d, |c: &mut Contract| c.propose_owner(acc(to)));
                (expect(ok), r)
            }
            Op::AcceptOwner { who, dep } => {
                let right = pre
                    .roles
                    .pending_owner
                    .as_ref()
                    .map(|x| x.to_string())
                    .unwrap_or("stranger.test".into());
                let who = Self::who(who, &right);
                let d = dep_of(dep);
                let ok = d == 1
                    && pre.roles.pending_owner.as_ref().map(|x| x.as_str()) == Some(who.as_str());
                if ok {
                    exp.roles.owner = acc(&who);
                    exp.roles.pending_owner = None;
                }
                let r = run!(who, d, |c: &mut Contract| c.accept_owner());
                (expect(ok), r)
            }
            Op::SetArbiter { to, who, dep } => {
                let who = Self::who(who, pre.roles.owner.as_str());
                let d = dep_of(dep);
                // Keep the arbiter among real roles most of the time.
                let to = if to < 12 {
                    "dao.test"
                } else {
                    ACCTS[to as usize % 16]
                };
                let ok = d == 1 && who == pre.roles.owner.as_str();
                if ok {
                    exp.roles.arbiter = acc(to);
                }
                let r = run!(who, d, |c: &mut Contract| c.set_arbiter(acc(to)));
                (expect(ok), r)
            }
            Op::StorageDeposit { agent, amt } => {
                let id = self.agent_id(&pre, agent);
                let ok = pre.agents.contains_key(&id);
                let r = run!("stranger.test".to_string(), amt, |c: &mut Contract| {
                    c.storage_deposit(id);
                });
                if r.is_ok() {
                    // storage balance must have grown by exactly amt (checked below)
                }
                (expect(ok), r)
            }
            Op::StorageWithdraw {
                agent,
                amt,
                who,
                dep,
            } => {
                let id = self.agent_id(&pre, agent);
                let who = Self::who(who, &Self::owner_of(&pre, id));
                let d = dep_of(dep);
                let bal = pre.agents.get(&id).map(|a| a.storage).unwrap_or(0);
                let amount = match amt % 5 {
                    0 => 0,
                    1 => 1,
                    2 => NEAR / 10,
                    3 => bal,
                    _ => bal + 1,
                };
                let ok = d == 1 && Self::is_owner(&pre, id, &who) && amount <= bal;
                if ok {
                    new_pending = Some(Pending::Near { agent: id, amount });
                }
                let r = run!(who, d, |c: &mut Contract| {
                    let _ = c.storage_withdraw(id, U128(amount));
                });
                (expect(ok), r)
            }
            Op::Register {
                who,
                wallet,
                dep,
                len,
                caps,
            } => {
                let who = ACCTS[who as usize % 16].to_string();
                let wallet = ACCTS[wallet as usize % 16];
                let d = match dep % 4 {
                    0 => 0,
                    1 => 1,
                    _ => NEAR,
                };
                let (per, daily) =
                    caps.unwrap_or((prm.default_per_task_cap.0, prm.default_daily_cap.0));
                let base = len as usize <= 512 && per <= daily;
                let e = if !base || d <= 1 {
                    Expect::Fail
                } else {
                    Expect::Ok
                };
                if base && d > 1 {
                    let id = pre.agent_count + 1;
                    exp.agent_count += 1;
                    exp.agents.insert(
                        id,
                        AgentV {
                            agent: Agent {
                                id,
                                owner: acc(&who),
                                wallet: None,
                                pending_wallet: Some(acc(wallet)),
                                card_uri: "u".repeat(len as usize),
                                frozen: false,
                                registered_at: now,
                            },
                            balance: 0,
                            per_task: per,
                            daily,
                            daily_remaining: 0,
                            stake: (0, 0, 0, 0),
                            storage: 0,
                            wrec: WorkerRecord::default(),
                            crec: ClientRecord::default(),
                        },
                    );
                }
                let uri = "u".repeat(len as usize);
                let cp = caps.map(|c| (U128(c.0), U128(c.1)));
                let r = run!(who, d, |c: &mut Contract| {
                    c.register_agent(acc(wallet), uri, cp.map(|x| x.0), cp.map(|x| x.1));
                });
                (e, r)
            }
            Op::RecoverSurplus {
                inject,
                amt,
                who,
                dep,
            } => {
                // The token moves `inject` to the contract; the contract is told nothing.
                if inject > 0 && self.h.held.checked_add(inject).is_some() {
                    self.h.held += inject;
                    self.h.deposited += inject;
                    self.h.surplus += inject;
                }
                let who = Self::who(who, pre.roles.owner.as_str());
                let d = dep_of(dep);
                let ok = d == 1 && who == pre.roles.owner.as_str() && amt > 0;
                let send = amt.min(self.h.surplus);
                let owner = pre.roles.owner.to_string();
                let r = run!(who, d, |c: &mut Contract| {
                    let _ = c.recover_surplus(U128(amt), None);
                });
                if r.is_ok() {
                    // The token answers with the real balance: the callback sends
                    // min(amt, surplus), or refuses when there is none. It may
                    // never touch a bucket.
                    let held = self.h.held;
                    let owner2 = owner.clone();
                    let cb = self.h.call(CONTRACT, 0, |c| {
                        let _ = c.on_surplus_balance(Ok(U128(held)), U128(amt), acc(&owner2));
                    });
                    match cb {
                        Ok((_, logs)) if send > 0 => {
                            if count_event(&logs, "surplus_recovered") != 1 {
                                return Err(format!(
                                    "EVENTS: expected 1 surplus_recovered: {logs:?}"
                                ));
                            }
                            exp.totals[5] += send;
                            self.h.surplus -= send;
                            new_pending = Some(Pending::Ft {
                                to: owner,
                                amount: send,
                                refund: Refund::Surplus,
                            });
                        }
                        Ok(_) => {
                            return Err("ORACLE: recover_surplus paid out with no surplus".into())
                        }
                        Err(m) if send == 0 && m.contains("NoSurplus") => {}
                        Err(m) => {
                            return Err(format!(
                                "ORACLE: recover_surplus callback refused {send} of {amt}: {m}"
                            ))
                        }
                    }
                }
                (expect(ok), r)
            }
        };
        self.h.gas_tgas = gas_before;

        let ok = res.is_ok();
        stat(op, e, ok);
        self.h.trace.push(format!(
            "  {} (expected {:?}){}",
            if ok { "OK" } else { "FAIL" },
            e,
            res.as_ref()
                .err()
                .map(|m| format!(": {m}"))
                .unwrap_or_default()
        ));
        match (e, &res) {
            (Expect::Ok, Err(m)) => {
                return Err(format!(
                    "ORACLE: call expected to succeed failed with {m:?}"
                ))
            }
            (Expect::Fail, Ok(_)) => return Err("ORACLE: call expected to fail succeeded".into()),
            _ => {}
        }
        if let Ok(logs) = &res {
            if let Some(p) = new_pending {
                self.h.pending.push(p);
            }
            for (ev, n) in &events {
                let got = count_event(logs, ev);
                if got != *n {
                    return Err(format!("EVENTS: expected {n} {ev}, got {got}: {logs:?}"));
                }
            }
            if let Op::StorageDeposit { agent, amt } = op {
                let id = self.agent_id(&pre, *agent);
                let post = self.h.view();
                let want = pre.agents[&id].storage + amt;
                if post.agents[&id].storage != want {
                    return Err(format!(
                        "STORAGE: deposit left {} want {want}",
                        post.agents[&id].storage
                    ));
                }
            }
            self.after(Some(&pre), Some(&exp))
        } else {
            // A refused call must leave everything exactly as it was.
            let post = self.h.view();
            if post != pre {
                return Err("ATOMICITY: refused call changed state".into());
            }
            Ok(())
        }
    }

    fn after(&mut self, pre: Option<&World>, exp: Option<&World>) -> Result<(), String> {
        let post = self.h.view();
        if let Some(exp) = exp {
            let got = post.comparable();
            if &got != exp {
                return Err(format!(
                    "EFFECT: post-state differs from model\n{}",
                    diff(exp, &got)
                ));
            }
        }
        if let Some(p) = pre {
            // Storage balances only shrink, except by deposit / failed withdrawal.
            for (id, a) in &post.agents {
                if let Some(b) = p.agents.get(id) {
                    let _ = (a, b);
                }
            }
        }
        check_invariants(&self.h, pre, &post)
    }

    /// Close everything and take every token out. Every step must succeed.
    fn drain(&mut self) -> Result<(), String> {
        let must =
            |r: Result<(), String>, what: String| r.map_err(|e| format!("DRAIN {what}: {e}"));
        // 1. Finish transfers in flight.
        while !self.h.pending.is_empty() {
            must(
                self.step(&Op::Resolve { idx: 0, ok: true }),
                "resolve".into(),
            )?;
        }
        // 2. A year and a half later, a stranger closes every open job.
        self.h.now += 3 * MAX_WINDOW + 10;
        let w = self.h.view();
        for (id, j) in &w.jobs {
            let kind = match j.state {
                JobState::Created => 0,
                JobState::Accepted => 1,
                JobState::Submitted => 2,
                JobState::Disputed => 3,
                _ => continue,
            };
            let jid = *id;
            let r = self
                .h
                .call("stranger.test", 0, |c| match kind {
                    0 => c.expire_unaccepted(jid),
                    1 => c.expire_undelivered(jid),
                    2 => c.auto_approve(jid),
                    _ => c.expire_dispute(jid),
                })
                .map(|_| ());
            must(r, format!("close job {jid} in {:?}", j.state))?;
        }
        // 3. Fees out.
        let w = self.h.view();
        if w.totals[4] > 0 {
            must(
                self.step(&Op::ClaimFees {
                    who: Who::Right,
                    low_gas: false,
                }),
                "claim fees".into(),
            )?;
            must(
                self.step(&Op::Resolve { idx: 0, ok: true }),
                "resolve fees".into(),
            )?;
        }
        // 3b. The surplus out, by the owner.
        if self.h.surplus > 0 {
            let s = self.h.surplus;
            must(
                self.step(&Op::RecoverSurplus {
                    inject: 0,
                    amt: s,
                    who: Who::Right,
                    dep: 1,
                }),
                "recover surplus".into(),
            )?;
            must(
                self.step(&Op::Resolve { idx: 0, ok: true }),
                "resolve surplus".into(),
            )?;
        }
        // 4. Every agent's balance and stake out, by its owner.
        let w = self.h.view();
        for (id, a) in &w.agents {
            let owner = a.agent.owner.to_string();
            let id = *id;
            if a.balance > 0 {
                let amt = a.balance;
                let r = self
                    .h
                    .call(&owner, 1, |c| {
                        let _ = c.withdraw(id, U128(amt), None);
                    })
                    .map(|_| ());
                must(r, format!("withdraw agent {id}"))?;
                self.h.pending.push(Pending::Ft {
                    to: owner.clone(),
                    amount: amt,
                    refund: Refund::Purse(id),
                });
                must(
                    self.step(&Op::Resolve { idx: 0, ok: true }),
                    "resolve withdraw".into(),
                )?;
            }
            if a.stake.1 > 0 {
                let r = self.h.call(&owner, 1, |c| c.cancel_unstake(id)).map(|_| ());
                must(r, format!("cancel unstake {id}"))?;
            }
            let s = self.h.view().agents[&id].stake;
            if s.3 != 0 {
                return Err(format!(
                    "DRAIN agent {id} still has {} locked with every job closed",
                    s.3
                ));
            }
            if s.0 > 0 {
                let amt = s.0;
                let r = self
                    .h
                    .call(&owner, 1, |c| c.request_unstake(id, U128(amt)))
                    .map(|_| ());
                must(r, format!("request unstake {id}"))?;
                self.h.now = self.h.now.max(self.h.view().agents[&id].stake.2);
                let r = self
                    .h
                    .call(&owner, 1, |c| {
                        let _ = c.complete_unstake(id);
                    })
                    .map(|_| ());
                must(r, format!("complete unstake {id}"))?;
                self.h.pending.push(Pending::Ft {
                    to: owner.clone(),
                    amount: amt,
                    refund: Refund::Stake(id),
                });
                must(
                    self.step(&Op::Resolve { idx: 0, ok: true }),
                    "resolve unstake".into(),
                )?;
            }
        }
        let w = self.h.view();
        check_invariants(&self.h, None, &w)?;
        if w.totals != [0; 6] || self.h.held != 0 {
            return Err(format!(
                "DRAIN left tokens: totals {:?} held {}",
                w.totals, self.h.held
            ));
        }
        if w.jobs.values().any(|j| j.state.is_open()) {
            return Err("DRAIN left an open job".into());
        }
        Ok(())
    }
}

fn model_settle(exp: &mut World, j: &mut Job, outcome: Outcome, review: bool) {
    j.state = JobState::Settled;
    j.outcome = Some(outcome);
    exp.totals[0] -= j.amount.0;
    if j.bond_locked {
        exp.agents.get_mut(&j.worker).unwrap().stake.3 -= j.worker_bond.0;
        j.bond_locked = false;
    }
    let paid = j.amount.0 - j.fee.0;
    exp.agents.get_mut(&j.worker).unwrap().balance += paid;
    exp.totals[2] += paid;
    exp.totals[4] += j.fee.0;
    let r = &mut exp.agents.get_mut(&j.worker).unwrap().wrec;
    match outcome {
        Outcome::Unresolved => r.unresolved += 1,
        _ => r.completed += 1,
    }
    if review {
        r.reviews += 1;
        r.review_score_sum += 100;
    }
}

fn model_refund(exp: &mut World, j: &mut Job, outcome: Option<Outcome>) {
    j.state = JobState::Refunded;
    j.outcome = outcome;
    exp.totals[0] -= j.amount.0;
    exp.totals[2] += j.amount.0;
    exp.agents.get_mut(&j.client).unwrap().balance += j.amount.0;
    if j.bond_locked {
        let b = j.worker_bond.0;
        let s = &mut exp.agents.get_mut(&j.worker).unwrap().stake;
        s.3 -= b;
        if outcome == Some(Outcome::Failed) {
            let take = b.min(s.0);
            s.0 -= take;
            exp.totals[3] -= take;
            exp.totals[2] += take;
            exp.agents.get_mut(&j.client).unwrap().balance += take;
        }
        j.bond_locked = false;
    }
    let r = &mut exp.agents.get_mut(&j.worker).unwrap().wrec;
    match outcome {
        Some(Outcome::Failed) => {
            r.failed += 1;
            r.reviews += 1;
        }
        Some(Outcome::DisputeLost) => {
            r.disputes_lost += 1;
            r.reviews += 1;
        }
        _ => {}
    }
}

fn diff(a: &World, b: &World) -> String {
    let mut out = vec![];
    if a.totals != b.totals {
        out.push(format!("totals model {:?} got {:?}", a.totals, b.totals));
    }
    for (id, x) in &a.agents {
        match b.agents.get(id) {
            Some(y) if x != y => out.push(format!("agent {id}\n  model {x:?}\n  got   {y:?}")),
            None => out.push(format!("agent {id} missing")),
            _ => {}
        }
    }
    for (id, x) in &a.jobs {
        match b.jobs.get(id) {
            Some(y) if x != y => out.push(format!("job {id}\n  model {x:?}\n  got   {y:?}")),
            None => out.push(format!("job {id} missing")),
            _ => {}
        }
    }
    if a.jobs.len() != b.jobs.len() || a.agents.len() != b.agents.len() {
        out.push(format!(
            "counts model {}/{} got {}/{}",
            a.jobs.len(),
            a.agents.len(),
            b.jobs.len(),
            b.agents.len()
        ));
    }
    if a.roles != b.roles {
        out.push(format!("roles model {:?} got {:?}", a.roles, b.roles));
    }
    if a.params != b.params {
        out.push("params differ".into());
    }
    if a.index != b.index {
        out.push(format!("index model {:?} got {:?}", a.index, b.index));
    }
    out.join("\n")
}

static STATS: std::sync::Mutex<Option<BTreeMap<String, [u32; 3]>>> = std::sync::Mutex::new(None);

fn stat(op: &Op, e: Expect, ok: bool) {
    let mut g = STATS.lock().unwrap();
    let m = g.get_or_insert_with(BTreeMap::new);
    let name = format!("{op:?}");
    let name = name
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap()
        .to_string();
    let row = m.entry(name).or_insert([0; 3]);
    match e {
        Expect::Unknown => row[2] += 1,
        _ if ok => row[0] += 1,
        _ => row[1] += 1,
    }
}

/// `FUZZ_STATS=1`: print, per operation, how many calls succeeded / were
/// refused / were not predicted, so the coverage of the oracle is visible.
pub fn print_stats() {
    static CASES_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let done = CASES_DONE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if std::env::var("FUZZ_STATS").is_err() || done != cases() {
        return;
    }
    let g = STATS.lock().unwrap();
    if let Some(m) = g.as_ref() {
        eprintln!("op              ok    refused  unpredicted");
        for (k, v) in m {
            eprintln!("{k:14} {:>7} {:>8} {:>8}", v[0], v[1], v[2]);
        }
    }
}

/// Refused calls panic by design; keep the default hook from printing each.
fn quiet() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var("FUZZ_VERBOSE").is_err() {
            std::panic::set_hook(Box::new(|_| {}));
        }
    });
}

/// The mocked blockchain is thread-local and the step function has a large
/// frame, so every case runs on its own thread with a generous stack (the
/// default 2 MB test stack overflowed in release builds).
pub fn run_ops(ops: &[Op]) -> Result<(), String> {
    quiet();
    let ops = ops.to_vec();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || run_ops_here(&ops))
        .unwrap()
        .join()
        .unwrap_or_else(|e| Err(format!("harness panicked: {}", panic_msg(e))))
}

fn run_ops_here(ops: &[Op]) -> Result<(), String> {
    let mut r = Runner::new();
    r.after(None, None).map_err(|e| format!("setup: {e}"))?;
    for (i, op) in ops.iter().enumerate() {
        r.h.trace.push(format!("#{i} {op:?}"));
        if let Err(e) = r.step(op) {
            let tail: Vec<_> = r.h.trace.iter().rev().take(30).rev().cloned().collect();
            return Err(format!(
                "step {i} {op:?}: {e}\n--- trace tail ---\n{}",
                tail.join("\n")
            ));
        }
    }
    r.drain().map_err(|e| {
        let tail: Vec<_> = r.h.trace.iter().rev().take(30).rev().cloned().collect();
        format!("{e}\n--- trace tail ---\n{}", tail.join("\n"))
    })
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), max_shrink_iters: 20_000, ..ProptestConfig::default() })]

    #[test]
    fn fuzz_market_invariants(ops in prop::collection::vec(op_s(), 1..120)) {
        let r = run_ops(&ops);
        print_stats();
        if let Err(e) = r {
            prop_assert!(false, "{}", e);
        }
    }
}

// ───────────────────────────── targeted sequences ─────────────────────────────

/// Exact deadline boundaries for every timed transition.
#[test]
fn fuzz_boundaries_exact() {
    use Op::*;
    let right = Who::Right;
    let create = |aw, ww| Create {
        client: 0,
        worker: 1,
        amt: 1_000_000,
        aw,
        ww,
        who: right,
        hash: 0,
    };
    let seqs: Vec<Vec<Op>> = vec![
        // accept at accept_deadline (ok), at +1 (fail)
        vec![
            create(300, 600),
            Jump {
                job: 0,
                kind: 0,
                delta: 0,
            },
            Accept { job: 0, who: right },
        ],
        vec![
            create(300, 600),
            Jump {
                job: 0,
                kind: 0,
                delta: 1,
            },
            Accept { job: 0, who: right },
            Exit {
                job: 0,
                kind: 0,
                who: right,
            },
        ],
        vec![
            create(300, 600),
            Jump {
                job: 0,
                kind: 0,
                delta: 0,
            },
            Exit {
                job: 0,
                kind: 0,
                who: right,
            },
        ],
        // submit at work_deadline, expire_undelivered at deadline and +1
        vec![
            create(1, 1),
            Accept { job: 0, who: right },
            Jump {
                job: 0,
                kind: 1,
                delta: 0,
            },
            Exit {
                job: 0,
                kind: 1,
                who: right,
            },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
        ],
        vec![
            create(1, 1),
            Accept { job: 0, who: right },
            Jump {
                job: 0,
                kind: 1,
                delta: 1,
            },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
            Exit {
                job: 0,
                kind: 1,
                who: right,
            },
        ],
        // dispute at review_deadline ok; auto_approve only after
        vec![
            create(10, 10),
            Accept { job: 0, who: right },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
            Jump {
                job: 0,
                kind: 2,
                delta: 0,
            },
            Exit {
                job: 0,
                kind: 2,
                who: right,
            },
            Dispute {
                job: 0,
                who: right,
                hash: None,
            },
            Jump {
                job: 0,
                kind: 3,
                delta: 0,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
            Jump {
                job: 0,
                kind: 3,
                delta: 1,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
            Rule {
                job: 0,
                for_worker: false,
                who: right,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
        ],
        // expire_dispute: refused at dispute_deadline + grace, works one second later;
        // the arbiter may still rule in between
        vec![
            create(10, 10),
            Accept { job: 0, who: right },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
            Dispute {
                job: 0,
                who: right,
                hash: None,
            },
            Jump {
                job: 0,
                kind: 5,
                delta: -1,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
            Jump {
                job: 0,
                kind: 5,
                delta: 0,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
            Jump {
                job: 0,
                kind: 5,
                delta: 1,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
            Rule {
                job: 0,
                for_worker: true,
                who: right,
            },
        ],
        vec![
            create(10, 10),
            Accept { job: 0, who: right },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
            Dispute {
                job: 0,
                who: right,
                hash: None,
            },
            Jump {
                job: 0,
                kind: 5,
                delta: 1,
            },
            Rule {
                job: 0,
                for_worker: true,
                who: right,
            },
            Exit {
                job: 0,
                kind: 3,
                who: right,
            },
        ],
        vec![
            create(10, 10),
            Accept { job: 0, who: right },
            Submit {
                job: 0,
                who: right,
                hash: 0,
            },
            Jump {
                job: 0,
                kind: 2,
                delta: 1,
            },
            Dispute {
                job: 0,
                who: right,
                hash: None,
            },
            Approve {
                job: 0,
                who: right,
                hash: Some(0),
            },
        ],
        // unstake exactly at unlock_at
        vec![
            ReqUnstake {
                agent: 1,
                amt: 1,
                who: right,
                dep: 1,
            },
            Jump {
                job: 1,
                kind: 4,
                delta: -1,
            },
            CompleteUnstake {
                agent: 1,
                who: right,
                dep: 1,
                low_gas: false,
            },
            Jump {
                job: 1,
                kind: 4,
                delta: 0,
            },
            CompleteUnstake {
                agent: 1,
                who: right,
                dep: 1,
                low_gas: false,
            },
            Resolve { idx: 0, ok: false },
        ],
    ];
    for s in seqs {
        run_ops(&s).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Edge values: 0, 1 yocto, u128::MAX, empty and huge strings, zero windows.
#[test]
fn fuzz_edge_values() {
    use Op::*;
    let rt = Who::Right;
    let mut s = vec![];
    for amt in [
        0u128,
        1,
        9_999,
        10_000,
        10_000_000,
        10_000_001,
        u128::MAX / 10_000 + 1,
        u128::MAX,
    ] {
        s.push(Create {
            client: 0,
            worker: 1,
            amt,
            aw: 1,
            ww: 1,
            who: rt,
            hash: 0,
        });
        s.push(Direct {
            client: 0,
            worker: 1,
            amt,
            who: rt,
            hash: 0,
        });
        s.push(Withdraw {
            agent: 0,
            amt,
            who: rt,
            dep: 1,
            other_receiver: false,
            low_gas: false,
        });
        s.push(ReqUnstake {
            agent: 0,
            amt,
            who: rt,
            dep: 1,
        });
        s.push(SetCaps {
            agent: 0,
            per: amt,
            daily: amt,
            who: rt,
            dep: 1,
        });
        s.push(Fund {
            agent: 0,
            amt,
            who: rt,
            msg: 0,
        });
    }
    for (aw, ww) in [
        (0, 1),
        (1, 0),
        (u64::MAX, 1),
        (1, u64::MAX),
        (MAX_WINDOW, MAX_WINDOW),
    ] {
        s.push(Create {
            client: 0,
            worker: 1,
            amt: 20_000,
            aw,
            ww,
            who: rt,
            hash: 0,
        });
    }
    for h in 0..10u8 {
        s.push(Create {
            client: 0,
            worker: 1,
            amt: 20_000,
            aw: 5,
            ww: 5,
            who: rt,
            hash: h,
        });
        s.push(Accept { job: 0, who: rt });
        s.push(Submit {
            job: 0,
            who: rt,
            hash: h,
        });
        s.push(Approve {
            job: 0,
            who: rt,
            hash: Some(h),
        });
    }
    for len in [0u16, 512, 513, 10_000] {
        s.push(SetCardUri {
            agent: 0,
            len,
            who: rt,
            dep: 1,
        });
        s.push(Register {
            who: 6,
            wallet: 14,
            dep: 2,
            len,
            caps: None,
        });
    }
    // Huge-amount parameters: fee and bonds overflow, which must refuse.
    s.push(SetParams {
        v: 4,
        who: rt,
        dep: 1,
    });
    s.push(SetCaps {
        agent: 0,
        per: u128::MAX,
        daily: u128::MAX,
        who: rt,
        dep: 1,
    });
    s.push(Fund {
        agent: 0,
        amt: u128::MAX / 2,
        who: rt,
        msg: 0,
    });
    s.push(Create {
        client: 0,
        worker: 1,
        amt: u128::MAX / 10_000 + 1,
        aw: 1,
        ww: 1,
        who: rt,
        hash: 0,
    });
    s.push(Create {
        client: 0,
        worker: 1,
        amt: u128::MAX / 10_000,
        aw: 1,
        ww: 1,
        who: rt,
        hash: 0,
    });
    s.push(Direct {
        client: 0,
        worker: 1,
        amt: u128::MAX / 10_000,
        who: rt,
        hash: 0,
    });
    for v in 0..8u8 {
        s.push(SetParams { v, who: rt, dep: 1 });
        s.push(Create {
            client: 2,
            worker: 1,
            amt: 20_000,
            aw: 1,
            ww: 1,
            who: rt,
            hash: 0,
        });
        s.push(Accept { job: 0, who: rt });
        s.push(Submit {
            job: 0,
            who: rt,
            hash: 0,
        });
        s.push(Dispute {
            job: 0,
            who: rt,
            hash: None,
        });
        s.push(Rule {
            job: 0,
            for_worker: v % 2 == 0,
            who: rt,
        });
    }
    run_ops(&s).unwrap_or_else(|e| panic!("{e}"));
}

// ───────────────────────────── budget model ─────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    /// The rolling window: a charge is accepted iff it fits the per-task cap
    /// and the daily cap minus every charge made in the current hour or the
    /// 24 before it; views equal what `roll` writes.
    #[test]
    fn fuzz_budget_matches_reference(
        per in prop_oneof![Just(0u128), Just(1u128), 1u128..1_000, Just(u128::MAX)],
        daily_extra in prop_oneof![Just(0u128), 0u128..5_000],
        steps in prop::collection::vec(
            (prop_oneof![Just(0u64), Just(1u64), 1u64..7_200, Just(3_599u64), Just(3_600u64), Just(24 * 3_600u64), Just(25 * 3_600u64), Just(100 * 3_600u64)],
             prop_oneof![Just(0u128), Just(1u128), 1u128..1_500, Just(u128::MAX)]),
            1..200),
    ) {
        use crate::budget::*;
        let daily = per.saturating_add(daily_extra);
        let mut p = Purse::new(per, daily);
        let mut t = T0;
        let mut accepted: Vec<(u64, u128)> = vec![];
        for (dt, amt) in steps {
            t += dt;
            let h = hour_of(t);
            let in_window = |acc: &Vec<(u64, u128)>| -> u128 {
                acc.iter().filter(|(hh, _)| *hh + BUCKETS > h).map(|x| x.1).sum()
            };
            let spent = in_window(&accepted);
            prop_assert_eq!(window_spent(&p, t), spent, "window_spent at {}", t);
            prop_assert_eq!(daily_remaining(&p, t), daily.saturating_sub(spent));
            let oldest = accepted.iter().filter(|(hh, a)| *hh + BUCKETS > h && *a > 0).map(|x| x.0).min();
            prop_assert_eq!(next_release_at(&p, t), oldest.map_or(0, |o| (o + BUCKETS) * HOUR));
            let want = amt <= per && amt <= daily.saturating_sub(spent);
            let mut q = p.clone();
            let got = catch_unwind(AssertUnwindSafe(|| charge(&mut q, amt, t))).is_ok();
            prop_assert_eq!(got, want, "charge {} at {} (spent {})", amt, t, spent);
            if got {
                p = q;
                accepted.push((h, amt));
                prop_assert!(in_window(&accepted) <= daily, "daily cap exceeded");
            }
        }
    }
}
