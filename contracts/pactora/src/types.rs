//! Stored and returned types. Amounts are `U128` so they cross JSON as
//! strings: a JS number would lose precision above 2^53.

use near_sdk::json_types::U128;
use near_sdk::{near, AccountId};

pub type AgentId = u64;
pub type JobId = u64;

/// Number of hourly buckets in the rolling 24 h window. 25, not 24, so a spend
/// counts for between 24 and 25 hours and never for less — the same algorithm
/// as AgentAccountCore on EVM.
pub const BUCKETS: u64 = 25;
pub const HOUR: u64 = 3_600;
/// Longest any window may be: one year.
pub const MAX_WINDOW: u64 = 365 * 24 * HOUR;
/// Longest a stake withdrawal may be delayed: 90 days.
pub const MAX_WITHDRAW_DELAY: u64 = 90 * 24 * HOUR;
pub const MAX_FEE_BPS: u16 = 1_000;
pub const BPS: u128 = 10_000;

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Created,
    Accepted,
    Submitted,
    Disputed,
    Settled,
    Refunded,
}

impl JobState {
    pub fn is_open(self) -> bool {
        matches!(
            self,
            JobState::Created | JobState::Accepted | JobState::Submitted | JobState::Disputed
        )
    }
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failed,
    DisputeLost,
    Unresolved,
}

/// Protocol parameters. Bounds are checked in `Contract::check_params`.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub protocol_fee_bps: u16,
    pub min_fee: U128,
    pub min_job_amount: U128,
    pub max_job_amount: U128,
    /// Largest `direct_pay`; 0 turns the fast path off.
    pub fast_path_max: U128,
    pub review_window: u64,
    pub max_accept_window: u64,
    pub max_work_window: u64,
    pub dispute_timeout: u64,
    pub bond_bps: u16,
    pub min_bond: U128,
    pub dispute_bond_bps: u16,
    pub min_dispute_bond: U128,
    pub min_stake: U128,
    pub withdraw_delay: u64,
    pub default_per_task_cap: U128,
    pub default_daily_cap: U128,
}

/// An agent in the registry. `owner` is the person (full-access key);
/// `wallet` is the account the agent signs with — normally a sub-account
/// holding only a function-call key scoped to this contract.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub id: AgentId,
    pub owner: AccountId,
    pub wallet: Option<AccountId>,
    pub pending_wallet: Option<AccountId>,
    pub card_uri: String,
    pub frozen: bool,
    pub registered_at: u64,
}

/// What the agent's wallet may spend, and what it has: the NEAR counterpart
/// of AgentAccount's policy, held inside the contract because a function-call
/// key cannot attach tokens.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Purse {
    pub balance: u128,
    pub per_task_cap: u128,
    pub daily_cap: u128,
    pub window_total: u128,
    pub last_hour: u64,
    pub buckets: Vec<u128>,
}

impl Purse {
    pub fn new(per_task_cap: u128, daily_cap: u128) -> Self {
        Self {
            balance: 0,
            per_task_cap,
            daily_cap,
            window_total: 0,
            last_hour: 0,
            buckets: vec![0; BUCKETS as usize],
        }
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stake {
    /// Stake that counts, including what is locked against open jobs.
    pub available: u128,
    /// Requested for withdrawal; does not count towards hireability.
    pub pending: u128,
    pub unlock_at: u64,
    /// Sum of the bonds of this agent's accepted, unfinished jobs.
    pub locked: u128,
}

impl Stake {
    pub fn unlocked(&self) -> u128 {
        self.available.saturating_sub(self.locked)
    }
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub client: AgentId,
    pub worker: AgentId,
    pub amount: U128,
    pub fee: U128,
    pub state: JobState,
    pub accept_deadline: u64,
    pub work_deadline: u64,
    pub review_deadline: u64,
    pub dispute_deadline: u64,
    pub review_window: u64,
    pub dispute_timeout: u64,
    pub spec_hash: String,
    pub result_hash: Option<String>,
    pub worker_bond: U128,
    pub dispute_bond: U128,
    /// True while `worker_bond` is locked in the worker's stake.
    pub bond_locked: bool,
    /// Hash of the reviewer's verdict (for example a TEE-attested judge
    /// response) recorded with `approve` or `dispute`.
    pub review_hash: Option<String>,
    pub outcome: Option<Outcome>,
    pub created_at: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkerRecord {
    pub completed: u64,
    pub failed: u64,
    pub disputes_lost: u64,
    pub unresolved: u64,
    pub reviews: u64,
    pub review_score_sum: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientRecord {
    pub paid: u64,
    pub disputes_rejected: u64,
    pub disputes_upheld: u64,
    pub silent: u64,
    pub reviews: u64,
    pub review_score_sum: u64,
}

/// Every token the contract holds is in exactly one of these buckets.
/// Invariant I1: token balance == sum of all fields.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub locked: u128,
    pub dispute_bonds: u128,
    pub balances: u128,
    pub stakes: u128,
    pub fees_owed: u128,
    pub in_flight: u128,
}

impl Totals {
    pub fn sum(&self) -> u128 {
        self.locked
            + self.dispute_bonds
            + self.balances
            + self.stakes
            + self.fees_owed
            + self.in_flight
    }
}

/// Who gets the tokens back if an outgoing transfer fails.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub enum Refund {
    Purse(AgentId),
    Fees,
}

// ─────────────────────────── views ───────────────────────────

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct PurseView {
    pub balance: U128,
    pub per_task_cap: U128,
    pub daily_cap: U128,
    pub spent_today: U128,
    pub daily_remaining: U128,
    /// When the oldest spend still counted leaves the window (seconds); 0 if none.
    pub next_release_at: u64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct StakeView {
    pub available: U128,
    pub pending: U128,
    pub unlock_at: u64,
    pub locked: U128,
    pub unlocked: U128,
    pub hireable: bool,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct TotalsView {
    pub locked: U128,
    pub dispute_bonds: U128,
    pub balances: U128,
    pub stakes: U128,
    pub fees_owed: U128,
    pub in_flight: U128,
    pub sum: U128,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Roles {
    pub owner: AccountId,
    pub pending_owner: Option<AccountId>,
    pub arbiter: AccountId,
    pub pauser: AccountId,
    pub fee_recipient: AccountId,
    pub token: AccountId,
    pub paused: bool,
}

/// Messages accepted by `ft_on_transfer`.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub enum TokenMsg {
    /// Add to the agent's spendable balance.
    Fund { agent_id: AgentId },
    /// Add to the agent's stake (makes it hireable, backs its bonds).
    Stake { agent_id: AgentId },
}
