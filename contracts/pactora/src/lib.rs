//! # Pactora on NEAR
//!
//! Escrow, bonds, reputation and spend caps for AI agents that hire each
//! other. One contract holds what took eight on EVM (TaskEscrow, StakeVault,
//! AgentAccount, the ERC-8004 registries, ClientReputationWriter,
//! SettlementValidator): NEAR cross-contract calls are asynchronous, and every
//! hiring rule — same owner, wallet, bond, caps — must be checked in one
//! synchronous step.
//!
//! ## Money model
//! Every token the contract holds sits in exactly one bucket of [`Totals`]
//! (invariant I1). Payments between agents are internal credits, so a
//! settlement can never fail half-way; tokens leave only through `withdraw`
//! and `claim_fees`, whose failure puts the amount back where it came from.
//!
//! ## Keys
//! An agent's wallet is a NEAR account holding a function-call key scoped to
//! this contract. Such a key cannot attach a deposit, so no method an agent
//! calls needs one, and every method that moves value out needs one yocto —
//! which only the owner's full-access key can attach.

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, BorshStorageKey, Gas, NearToken,
    PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};

pub mod budget;
pub mod errors;
pub mod events;
pub mod types;

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod tests;

use errors::*;
use types::*;

/// Gas for the outgoing `ft_transfer` and for our callback after it.
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_ON_SEND: Gas = Gas::from_tgas(10);
/// Gas a sending method must still have left: the transfer, the callback,
/// and 10 Tgas for promise creation and the rest of the call.
const GAS_SEND_MIN: Gas = Gas::from_tgas(30);
/// `recover_surplus`: the token's `ft_balance_of`, then our callback, which
/// itself starts a send (so it needs `GAS_SEND_MIN` left after its own work).
const GAS_FT_BALANCE_OF: Gas = Gas::from_tgas(5);
const GAS_ON_SURPLUS: Gas = Gas::from_tgas(50);
const GAS_RECOVER_MIN: Gas = Gas::from_tgas(70);
/// Layout of the state blob (`STATE`); bumped with `migrate`.
const STATE_VERSION: u8 = 1;
/// Where near-sdk keeps the contract state.
const STATE_KEY: &[u8] = b"STATE";
/// `dispute_grace` given to a state migrated from the layout before 1.1.0.
const DEFAULT_DISPUTE_GRACE: u64 = 24 * HOUR;
const MAX_CARD_URI: usize = 512;
/// Bytes a job may still grow by after creation (result hash, review hash,
/// outcome), reserved and billed to the client up front so no later call by
/// anyone leaves the contract paying for an agent's storage.
const JOB_GROWTH_BYTES: u64 = 2 * (4 + 64) + 2 + 16;

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    Agents,
    AgentByWallet,
    Purses,
    Stakes,
    StorageBalances,
    Jobs,
    WorkerRecords,
    ClientRecords,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    /// `STATE_VERSION` of this layout; `migrate` moves older ones forward.
    version: u8,
    owner: AccountId,
    pending_owner: Option<AccountId>,
    arbiter: AccountId,
    pauser: AccountId,
    fee_recipient: AccountId,
    token: AccountId,
    paused: bool,
    params: Params,

    next_agent_id: AgentId,
    next_job_id: JobId,

    agents: LookupMap<AgentId, Agent>,
    agent_by_wallet: LookupMap<AccountId, AgentId>,
    purses: LookupMap<AgentId, Purse>,
    stakes: LookupMap<AgentId, Stake>,
    /// NEAR (yocto) each agent prepaid for the storage its jobs use.
    storage_balances: LookupMap<AgentId, u128>,
    jobs: LookupMap<JobId, Job>,
    worker_records: LookupMap<AgentId, WorkerRecord>,
    client_records: LookupMap<AgentId, ClientRecord>,

    totals: Totals,
}

fn now_s() -> u64 {
    env::block_timestamp() / 1_000_000_000
}

fn check_hash(h: &str) {
    require!(
        h.len() == 64
            && h.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        BAD_HASH
    );
}

/// Gas left (not gas attached) must cover the transfer, the callback and
/// the rest of this call.
fn require_send_gas() {
    let left = env::prepaid_gas().saturating_sub(env::used_gas());
    require!(left >= GAS_SEND_MIN, NOT_ENOUGH_GAS);
}

fn bps_of(amount: u128, bps: u16) -> u128 {
    amount * bps as u128 / BPS
}

#[near]
impl Contract {
    // ═══════════════════════════ setup and roles ═══════════════════════════

    /// Only the contract account itself may initialise: deploy and `new` go
    /// in one batch signed by that account, so nobody can call `new` on
    /// freshly deployed code with roles of their choosing.
    #[init]
    #[private]
    pub fn new(
        owner: AccountId,
        token: AccountId,
        arbiter: AccountId,
        pauser: AccountId,
        fee_recipient: AccountId,
        params: Params,
    ) -> Self {
        Self::check_params(&params);
        Self {
            version: STATE_VERSION,
            owner,
            pending_owner: None,
            arbiter,
            pauser,
            fee_recipient,
            token,
            paused: false,
            params,
            next_agent_id: 1,
            next_job_id: 1,
            agents: LookupMap::new(StorageKey::Agents),
            agent_by_wallet: LookupMap::new(StorageKey::AgentByWallet),
            purses: LookupMap::new(StorageKey::Purses),
            stakes: LookupMap::new(StorageKey::Stakes),
            storage_balances: LookupMap::new(StorageKey::StorageBalances),
            jobs: LookupMap::new(StorageKey::Jobs),
            worker_records: LookupMap::new(StorageKey::WorkerRecords),
            client_records: LookupMap::new(StorageKey::ClientRecords),
            totals: Totals::default(),
        }
    }

    /// Move the state written by the previous code (1.0.0: no `version`, no
    /// `dispute_grace`) to this layout. Called by the contract account in
    /// the same batch as the redeploy. Reading the old layout fails on a
    /// state already migrated, so it cannot run twice. The maps are keyed
    /// by prefix and are untouched.
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let bytes =
            env::storage_read(STATE_KEY).unwrap_or_else(|| env::panic_str(migration::NOT_V0));
        let old = migration::V0::try_from_slice(&bytes)
            .unwrap_or_else(|_| env::panic_str(migration::NOT_V0));
        let p = old.params;
        Self {
            version: STATE_VERSION,
            owner: old.owner,
            pending_owner: old.pending_owner,
            arbiter: old.arbiter,
            pauser: old.pauser,
            fee_recipient: old.fee_recipient,
            token: old.token,
            paused: old.paused,
            params: Params {
                protocol_fee_bps: p.protocol_fee_bps,
                min_fee: p.min_fee,
                min_job_amount: p.min_job_amount,
                max_job_amount: p.max_job_amount,
                fast_path_max: p.fast_path_max,
                review_window: p.review_window,
                max_accept_window: p.max_accept_window,
                max_work_window: p.max_work_window,
                dispute_timeout: p.dispute_timeout,
                dispute_grace: DEFAULT_DISPUTE_GRACE,
                bond_bps: p.bond_bps,
                min_bond: p.min_bond,
                dispute_bond_bps: p.dispute_bond_bps,
                min_dispute_bond: p.min_dispute_bond,
                min_stake: p.min_stake,
                withdraw_delay: p.withdraw_delay,
                default_per_task_cap: p.default_per_task_cap,
                default_daily_cap: p.default_daily_cap,
            },
            next_agent_id: old.next_agent_id,
            next_job_id: old.next_job_id,
            agents: old.agents,
            agent_by_wallet: old.agent_by_wallet,
            purses: old.purses,
            stakes: old.stakes,
            storage_balances: old.storage_balances,
            jobs: old.jobs,
            worker_records: old.worker_records,
            client_records: old.client_records,
            totals: old.totals,
        }
    }

    /// Two-step ownership transfer, so a typo cannot hand the contract away.
    #[payable]
    pub fn propose_owner(&mut self, new_owner: AccountId) {
        assert_one_yocto();
        self.only_owner();
        self.pending_owner = Some(new_owner);
    }

    #[payable]
    pub fn accept_owner(&mut self) {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        require!(
            self.pending_owner.as_ref() == Some(&caller),
            NOT_PENDING_OWNER
        );
        self.owner = caller;
        self.pending_owner = None;
        events::role_set("owner", &self.owner);
    }

    #[payable]
    pub fn set_arbiter(&mut self, arbiter: AccountId) {
        assert_one_yocto();
        self.only_owner();
        self.arbiter = arbiter;
        events::role_set("arbiter", &self.arbiter);
    }

    #[payable]
    pub fn set_pauser(&mut self, pauser: AccountId) {
        assert_one_yocto();
        self.only_owner();
        self.pauser = pauser;
        events::role_set("pauser", &self.pauser);
    }

    #[payable]
    pub fn set_fee_recipient(&mut self, fee_recipient: AccountId) {
        assert_one_yocto();
        self.only_owner();
        self.fee_recipient = fee_recipient;
        events::role_set("fee_recipient", &self.fee_recipient);
    }

    /// New parameters apply to new jobs only: each job copies its windows,
    /// fee and bonds when it is created.
    #[payable]
    pub fn set_params(&mut self, params: Params) {
        assert_one_yocto();
        self.only_owner();
        Self::check_params(&params);
        self.params = params;
        events::params_set();
    }

    /// Stops new hires only. Every exit — accept, submit, approve, dispute,
    /// expiry, withdrawal — stays open, so no money is ever trapped by a pause.
    /// No deposit needed: a pauser may hold only a function-call key, so an
    /// emergency stop is one fast call.
    pub fn pause(&mut self) {
        let caller = env::predecessor_account_id();
        require!(caller == self.pauser || caller == self.owner, NOT_PAUSER);
        self.paused = true;
        events::paused(&caller, true);
    }

    #[payable]
    pub fn unpause(&mut self) {
        assert_one_yocto();
        self.only_owner();
        self.paused = false;
        events::paused(&env::predecessor_account_id(), false);
    }

    // ═══════════════════════════ registry ═══════════════════════════

    /// Register an agent. Called by its owner, who attaches NEAR for storage;
    /// what the registration itself does not use becomes the agent's storage
    /// balance for future jobs. The wallet must then call `accept_wallet`
    /// itself: an agent cannot claim someone else's account.
    #[payable]
    pub fn register_agent(
        &mut self,
        wallet: AccountId,
        card_uri: String,
        per_task_cap: Option<U128>,
        daily_cap: Option<U128>,
    ) -> AgentId {
        require!(card_uri.len() <= MAX_CARD_URI, CARD_URI_TOO_LONG);
        let owner = env::predecessor_account_id();
        let before = env::storage_usage();

        let id = self.next_agent_id;
        self.next_agent_id += 1;
        let per_task = per_task_cap.map_or(self.params.default_per_task_cap.0, |c| c.0);
        let daily = daily_cap.map_or(self.params.default_daily_cap.0, |c| c.0);
        require!(per_task <= daily, INVALID_CAPS);

        self.agents.insert(
            id,
            Agent {
                id,
                owner: owner.clone(),
                wallet: None,
                pending_wallet: Some(wallet.clone()),
                card_uri: card_uri.clone(),
                frozen: false,
                registered_at: now_s(),
            },
        );
        self.purses.insert(id, Purse::new(per_task, daily));
        self.stakes.insert(id, Stake::default());
        self.worker_records.insert(id, WorkerRecord::default());
        self.client_records.insert(id, ClientRecord::default());
        self.storage_balances.insert(id, 0);
        self.flush();

        let cost = self.storage_cost_since(before);
        let attached = env::attached_deposit().as_yoctonear();
        require!(attached >= cost, INSUFFICIENT_STORAGE);
        self.storage_balances.insert(id, attached - cost);

        events::agent_registered(id, &owner, &wallet, &card_uri);
        events::caps_set(id, per_task, daily);
        id
    }

    /// The pending wallet accepts, proving the account agrees to act for the
    /// agent (the NEAR counterpart of ERC-8004 `setAgentWallet`'s signature).
    pub fn accept_wallet(&mut self, agent_id: AgentId) {
        let caller = env::predecessor_account_id();
        let mut agent = self.agent(agent_id);
        require!(
            agent.pending_wallet.as_ref() == Some(&caller),
            NOT_PENDING_WALLET
        );
        require!(!self.agent_by_wallet.contains_key(&caller), WALLET_TAKEN);
        let before = env::storage_usage();
        let previous = agent.wallet.take();
        if let Some(old) = &previous {
            self.agent_by_wallet.remove(old);
        }
        agent.wallet = Some(caller.clone());
        agent.pending_wallet = None;
        self.agents.insert(agent_id, agent);
        self.agent_by_wallet.insert(caller.clone(), agent_id);
        // The new wallet index entry is billed to the agent, not the contract.
        self.charge_storage(agent_id, before, 0);
        events::wallet_accepted(agent_id, &caller, previous.as_ref());
    }

    /// Owner proposes a new wallet (key rotation); the old one keeps working
    /// until the new one accepts. `None` withdraws a proposal, for example
    /// one made to a mistyped account.
    #[payable]
    pub fn propose_wallet(&mut self, agent_id: AgentId, wallet: Option<AccountId>) {
        assert_one_yocto();
        let mut agent = self.owned_agent(agent_id);
        let before = env::storage_usage();
        agent.pending_wallet = wallet.clone();
        self.agents.insert(agent_id, agent);
        self.charge_storage(agent_id, before, 0);
        events::wallet_proposed(agent_id, wallet.as_ref());
    }

    #[payable]
    pub fn set_card_uri(&mut self, agent_id: AgentId, card_uri: String) {
        assert_one_yocto();
        require!(card_uri.len() <= MAX_CARD_URI, CARD_URI_TOO_LONG);
        let mut agent = self.owned_agent(agent_id);
        let before = env::storage_usage();
        agent.card_uri = card_uri;
        self.agents.insert(agent_id, agent);
        self.charge_storage(agent_id, before, 0);
    }

    /// Kill switch: a frozen agent's wallet can do nothing and it cannot be
    /// hired. Exits that anyone may call still work, so open jobs still end.
    /// No deposit needed, so the owner can stop an agent with any key.
    pub fn freeze_agent(&mut self, agent_id: AgentId) {
        let mut agent = self.owned_agent(agent_id);
        agent.frozen = true;
        self.agents.insert(agent_id, agent);
        events::agent_frozen(agent_id, true);
    }

    #[payable]
    pub fn unfreeze_agent(&mut self, agent_id: AgentId) {
        assert_one_yocto();
        let mut agent = self.owned_agent(agent_id);
        agent.frozen = false;
        self.agents.insert(agent_id, agent);
        events::agent_frozen(agent_id, false);
    }

    #[payable]
    pub fn set_caps(&mut self, agent_id: AgentId, per_task_cap: U128, daily_cap: U128) {
        assert_one_yocto();
        self.owned_agent(agent_id);
        require!(per_task_cap.0 <= daily_cap.0, INVALID_CAPS);
        let mut purse = self.purse(agent_id);
        purse.per_task_cap = per_task_cap.0;
        purse.daily_cap = daily_cap.0;
        self.purses.insert(agent_id, purse);
        events::caps_set(agent_id, per_task_cap.0, daily_cap.0);
    }

    /// Anyone may prepay storage for an agent.
    #[payable]
    pub fn storage_deposit(&mut self, agent_id: AgentId) -> U128 {
        self.agent(agent_id);
        let b = self.storage_balance(agent_id) + env::attached_deposit().as_yoctonear();
        self.storage_balances.insert(agent_id, b);
        U128(b)
    }

    #[payable]
    pub fn storage_withdraw(&mut self, agent_id: AgentId, amount: U128) -> Promise {
        assert_one_yocto();
        let agent = self.owned_agent(agent_id);
        let b = self.storage_balance(agent_id);
        require!(amount.0 <= b, INSUFFICIENT_STORAGE);
        self.storage_balances.insert(agent_id, b - amount.0);
        Promise::new(agent.owner)
            .transfer(NearToken::from_yoctonear(amount.0))
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_ON_SEND)
                    .on_storage_sent(agent_id, amount),
            )
    }

    /// A NEAR transfer fails only if the owner account no longer exists; the
    /// amount then comes back to the contract and to the agent's balance.
    #[private]
    pub fn on_storage_sent(&mut self, agent_id: AgentId, amount: U128) -> bool {
        let ok = near_sdk::is_promise_success();
        if !ok {
            let b = self.storage_balance(agent_id);
            self.storage_balances.insert(agent_id, b + amount.0);
        }
        ok
    }

    // ═══════════════════════════ money out ═══════════════════════════

    /// Owner withdraws from the agent's balance. Needs the owner's full-access
    /// key (one yocto), so a leaked agent key can never take money out.
    #[payable]
    pub fn withdraw(
        &mut self,
        agent_id: AgentId,
        amount: U128,
        receiver: Option<AccountId>,
    ) -> Promise {
        assert_one_yocto();
        require_send_gas();
        let agent = self.owned_agent(agent_id);
        require!(amount.0 > 0, ZERO_AMOUNT);
        let mut purse = self.purse(agent_id);
        require!(purse.balance >= amount.0, INSUFFICIENT_BALANCE);
        purse.balance -= amount.0;
        self.purses.insert(agent_id, purse);
        self.totals.balances -= amount.0;
        self.send(
            receiver.unwrap_or(agent.owner),
            amount.0,
            Refund::Purse(agent_id),
        )
    }

    /// Anyone may push accumulated fees to the fee recipient.
    pub fn claim_fees(&mut self) -> Promise {
        require_send_gas();
        let amount = self.totals.fees_owed;
        require!(amount > 0, NOTHING_OWED);
        self.totals.fees_owed = 0;
        self.send(self.fee_recipient.clone(), amount, Refund::Fees)
    }

    /// After `ft_transfer`, which returns nothing: only success or failure
    /// matters, read with `is_promise_success`.
    #[private]
    pub fn on_send(&mut self, receiver: AccountId, amount: U128, refund: Refund) -> bool {
        self.totals.in_flight -= amount.0;
        let ok = near_sdk::is_promise_success();
        match (&refund, ok) {
            // The stake left only once the payout has landed.
            (Refund::Stake(id), true) => events::unstaked(*id, amount.0),
            // Back into stake, where the owner can request it again.
            (Refund::Stake(id), false) => {
                let mut s = self.stake(*id);
                s.available += amount.0;
                self.stakes.insert(*id, s);
                self.totals.stakes += amount.0;
                events::stake_restored(*id, amount.0);
            }
            (Refund::Purse(id), false) => {
                // The purse always exists: agents are never deleted.
                let mut purse = self.purse(*id);
                purse.balance += amount.0;
                self.purses.insert(*id, purse);
                self.totals.balances += amount.0;
            }
            (Refund::Fees, false) => self.totals.fees_owed += amount.0,
            // A surplus was never in any bucket: if it did not leave, it is
            // still a surplus, recoverable again.
            (Refund::Surplus, _) | (Refund::Purse(_), true) | (Refund::Fees, true) => {}
        }
        events::transfer_finished(&receiver, amount.0, &refund, ok);
        ok
    }

    /// Owner recovers tokens the contract holds beyond what it owes — a plain
    /// `ft_transfer` into the contract, which credits nobody (see
    /// SECURITY.md). Two steps: the token's `ft_balance_of`, then
    /// `on_surplus_balance` sends `min(amount, balance − totals.sum)`
    /// through the usual `in_flight` / `on_send` path. It can never reach
    /// what the protocol owes: everything owed is in `totals`, and the
    /// surplus is what is left above them.
    #[payable]
    pub fn recover_surplus(&mut self, amount: U128, receiver: Option<AccountId>) -> Promise {
        assert_one_yocto();
        self.only_owner();
        require!(amount.0 > 0, ZERO_AMOUNT);
        let left = env::prepaid_gas().saturating_sub(env::used_gas());
        require!(left >= GAS_RECOVER_MIN, NOT_ENOUGH_GAS);
        ext_ft_core::ext(self.token.clone())
            .with_static_gas(GAS_FT_BALANCE_OF)
            .ft_balance_of(env::current_account_id())
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_ON_SURPLUS)
                    .on_surplus_balance(amount, receiver.unwrap_or(self.owner.clone())),
            )
    }

    /// The balance was read after every transfer the contract had started
    /// (receipts to the token execute in order), and a send keeps its amount
    /// in `in_flight` until the token has moved it, so `balance − sum` never
    /// overstates the surplus at the moment it is read.
    #[private]
    pub fn on_surplus_balance(
        &mut self,
        #[callback_result] balance: Result<U128, PromiseError>,
        amount: U128,
        receiver: AccountId,
    ) -> Promise {
        let balance = balance.unwrap_or_else(|_| env::panic_str(BALANCE_UNKNOWN));
        let surplus = balance.0.saturating_sub(self.totals.sum());
        let send = amount.0.min(surplus);
        require!(send > 0, NO_SURPLUS);
        events::surplus_recovered(&receiver, send, surplus);
        self.send(receiver, send, Refund::Surplus)
    }

    // ═══════════════════════════ stake ═══════════════════════════

    #[payable]
    pub fn request_unstake(&mut self, agent_id: AgentId, amount: U128) {
        assert_one_yocto();
        self.owned_agent(agent_id);
        require!(amount.0 > 0, ZERO_AMOUNT);
        let mut s = self.stake(agent_id);
        require!(s.pending == 0, WITHDRAW_ALREADY_PENDING);
        require!(s.unlocked() >= amount.0, INSUFFICIENT_BOND);
        s.available -= amount.0;
        s.pending = amount.0;
        s.unlock_at = now_s() + self.params.withdraw_delay;
        events::unstake_requested(agent_id, amount.0, s.unlock_at);
        self.stakes.insert(agent_id, s);
    }

    #[payable]
    pub fn cancel_unstake(&mut self, agent_id: AgentId) {
        assert_one_yocto();
        self.owned_agent(agent_id);
        let mut s = self.stake(agent_id);
        require!(s.pending > 0, NOTHING_PENDING);
        let amount = s.pending;
        s.available += amount;
        s.pending = 0;
        s.unlock_at = 0;
        self.stakes.insert(agent_id, s);
        events::unstake_cancelled(agent_id, amount);
    }

    /// Pays matured stake to the owner, as v3 `StakeVault.withdraw` does. It
    /// never passes through the agent's balance, which the agent key could
    /// spend.
    #[payable]
    pub fn complete_unstake(&mut self, agent_id: AgentId) -> Promise {
        assert_one_yocto();
        require_send_gas();
        let agent = self.owned_agent(agent_id);
        let mut s = self.stake(agent_id);
        require!(s.pending > 0, NOTHING_PENDING);
        require!(now_s() >= s.unlock_at, STILL_LOCKED);
        let amount = s.pending;
        s.pending = 0;
        s.unlock_at = 0;
        self.stakes.insert(agent_id, s);
        self.totals.stakes -= amount;
        // `unstaked` is emitted by `on_send`, once the payout has landed.
        self.send(agent.owner, amount, Refund::Stake(agent_id))
    }

    // ═══════════════════════════ escrow ═══════════════════════════

    /// Client hires worker; `amount` moves from the client's balance into
    /// escrow, under the client's caps.
    pub fn create_job(
        &mut self,
        client_id: AgentId,
        worker_id: AgentId,
        amount: U128,
        spec_hash: String,
        accept_window: u64,
        work_window: u64,
    ) -> JobId {
        require!(!self.paused, PAUSED);
        let amount = amount.0;
        self.validate_hire(client_id, worker_id, amount);
        require!(amount <= self.params.max_job_amount.0, AMOUNT_ABOVE_MAXIMUM);
        require!(accept_window > 0 && work_window > 0, INVALID_WINDOW);
        require!(
            accept_window <= self.params.max_accept_window
                && work_window <= self.params.max_work_window,
            WINDOW_TOO_LONG
        );
        check_hash(&spec_hash);

        let before = env::storage_usage();
        self.spend(client_id, amount);
        self.totals.locked += amount;

        let now = now_s();
        let id = self.next_job_id;
        self.next_job_id += 1;
        let fee = self.fee_of(amount);
        let accept_deadline = now + accept_window;
        let job = Job {
            id,
            client: client_id,
            worker: worker_id,
            amount: U128(amount),
            fee: U128(fee),
            state: JobState::Created,
            accept_deadline,
            // Absolute from the start, so the worker knows its real deadline
            // before accepting rather than after.
            work_deadline: accept_deadline + work_window,
            review_deadline: 0,
            dispute_deadline: 0,
            review_window: self.params.review_window,
            dispute_timeout: self.params.dispute_timeout,
            spec_hash: spec_hash.clone(),
            result_hash: None,
            worker_bond: U128(self.bond_of(amount)),
            dispute_bond: U128(self.dispute_bond_of(amount)),
            bond_locked: false,
            review_hash: None,
            outcome: None,
            created_at: now,
        };
        events::job_created(
            id,
            client_id,
            worker_id,
            amount,
            fee,
            &spec_hash,
            accept_deadline,
            job.work_deadline,
        );
        self.jobs.insert(id, job);
        self.charge_storage(client_id, before, JOB_GROWTH_BYTES);
        id
    }

    /// Worker accepts and locks its bond. Allowed while paused: it is not a
    /// new hire. Hireability is not re-checked — only that the unlocked stake
    /// covers the bond, the same as v3.
    pub fn accept_job(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        self.only_wallet_of(job.worker);
        require!(job.state == JobState::Created, INVALID_STATE);
        require!(now_s() <= job.accept_deadline, DEADLINE_PASSED);
        let bond = job.worker_bond.0;
        if bond > 0 {
            let mut s = self.stake(job.worker);
            require!(s.unlocked() >= bond, INSUFFICIENT_BOND);
            // The bond counts toward the worker's own caps: a stolen wallet
            // key could otherwise accept jobs it never delivers and hand the
            // whole stake to an accomplice client through expire_undelivered.
            let mut p = self.purse(job.worker);
            budget::charge(&mut p, bond, now_s());
            self.purses.insert(job.worker, p);
            s.locked += bond;
            self.stakes.insert(job.worker, s);
            job.bond_locked = true;
        }
        job.state = JobState::Accepted;
        events::job_accepted(job_id, job.worker, bond, job.work_deadline);
        self.jobs.insert(job_id, job);
    }

    pub fn submit_result(&mut self, job_id: JobId, result_hash: String) {
        let mut job = self.job(job_id);
        self.only_wallet_of(job.worker);
        require!(job.state == JobState::Accepted, INVALID_STATE);
        require!(now_s() <= job.work_deadline, DEADLINE_PASSED);
        check_hash(&result_hash);
        job.state = JobState::Submitted;
        job.review_deadline = now_s() + job.review_window;
        events::result_submitted(job_id, &result_hash, job.review_deadline);
        job.result_hash = Some(result_hash);
        self.jobs.insert(job_id, job);
    }

    /// Client accepts the work. No deadline: it works after the review window
    /// too, until someone calls `auto_approve`.
    pub fn approve(&mut self, job_id: JobId, review_hash: Option<String>) {
        let mut job = self.job(job_id);
        self.only_wallet_of(job.client);
        require!(job.state == JobState::Submitted, INVALID_STATE);
        if let Some(h) = &review_hash {
            check_hash(h);
        }
        job.review_hash = review_hash;
        self.bump_client(job.client, |r| r.paid += 1);
        self.settle(&mut job, Outcome::Success, true);
        self.rate_client(&job, 100);
        self.jobs.insert(job_id, job);
    }

    /// Client disputes inside the review window, paying the dispute bond from
    /// its balance. The bond is outside the caps: it comes back unless the
    /// dispute is ruled against, and a client that spent its whole day on
    /// the hire must still be able to contest the result (a cap refusal
    /// here would hand the worker an auto-approval).
    pub fn dispute(&mut self, job_id: JobId, reason_hash: Option<String>) {
        let mut job = self.job(job_id);
        self.only_wallet_of(job.client);
        require!(job.state == JobState::Submitted, INVALID_STATE);
        require!(now_s() <= job.review_deadline, DEADLINE_PASSED);
        if let Some(h) = &reason_hash {
            check_hash(h);
        }
        let bond = job.dispute_bond.0;
        if bond > 0 {
            self.take(job.client, bond);
            self.totals.dispute_bonds += bond;
        }
        job.state = JobState::Disputed;
        job.dispute_deadline = now_s() + job.dispute_timeout;
        events::job_disputed(job_id, bond, job.dispute_deadline, reason_hash.as_deref());
        job.review_hash = reason_hash;
        self.jobs.insert(job_id, job);
    }

    /// The arbiter (a Sputnik DAO) rules. No deadline: it may rule until the
    /// moment `expire_dispute` actually executes, which cannot be before
    /// `dispute_deadline + dispute_grace`, so a ruling that lands late
    /// cannot be raced by a party expiring the dispute in the same block.
    pub fn resolve_dispute(&mut self, job_id: JobId, for_worker: bool) {
        require!(env::predecessor_account_id() == self.arbiter, NOT_ARBITER);
        let mut job = self.job(job_id);
        require!(job.state == JobState::Disputed, INVALID_STATE);
        let bond = job.dispute_bond.0;
        self.totals.dispute_bonds -= bond;
        events::dispute_resolved(job_id, for_worker, &self.arbiter.clone());
        if for_worker {
            self.bump_client(job.client, |r| r.disputes_rejected += 1);
            self.credit(job.worker, bond);
            if bond > 0 {
                events::dispute_bond_settled(job_id, job.worker, bond);
            }
            self.settle(&mut job, Outcome::Success, true);
            self.rate_client(&job, 0);
        } else {
            self.bump_client(job.client, |r| r.disputes_upheld += 1);
            self.credit(job.client, bond);
            if bond > 0 {
                events::dispute_bond_settled(job_id, job.client, bond);
            }
            self.refund(&mut job, "dispute", Some(Outcome::DisputeLost));
            self.rate_client(&job, 100);
        }
        self.jobs.insert(job_id, job);
    }

    /// Client withdraws an offer nobody accepted — even after the deadline.
    pub fn cancel(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        self.only_wallet_of(job.client);
        require!(job.state == JobState::Created, INVALID_STATE);
        self.refund(&mut job, "cancelled", None);
        self.jobs.insert(job_id, job);
    }

    // ── exits anyone may call (invariant I5: no open state lasts forever) ──

    pub fn expire_unaccepted(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        require!(job.state == JobState::Created, INVALID_STATE);
        require!(now_s() > job.accept_deadline, DEADLINE_NOT_PASSED);
        self.refund(&mut job, "unaccepted", None);
        self.jobs.insert(job_id, job);
    }

    /// The worker accepted and never delivered: refund the client and give it
    /// the worker's bond.
    pub fn expire_undelivered(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        require!(job.state == JobState::Accepted, INVALID_STATE);
        require!(now_s() > job.work_deadline, DEADLINE_NOT_PASSED);
        self.refund(&mut job, "undelivered", Some(Outcome::Failed));
        self.jobs.insert(job_id, job);
    }

    /// The client stayed silent through the review window: the worker is paid.
    pub fn auto_approve(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        require!(job.state == JobState::Submitted, INVALID_STATE);
        require!(now_s() > job.review_deadline, DEADLINE_NOT_PASSED);
        self.bump_client(job.client, |r| r.silent += 1);
        self.settle(&mut job, Outcome::Success, true);
        self.rate_client(&job, 50);
        self.jobs.insert(job_id, job);
    }

    /// The arbiter never ruled, and the grace after the deadline has passed
    /// too: the worker is paid the job, and the client's bond is split —
    /// half to the worker for the wait it was put through, half back to the
    /// client. Nobody is rated. The grace is read from the current params
    /// (not copied into the job), so the owner can lengthen it for disputes
    /// already open.
    pub fn expire_dispute(&mut self, job_id: JobId) {
        let mut job = self.job(job_id);
        require!(job.state == JobState::Disputed, INVALID_STATE);
        require!(
            now_s() > job.dispute_deadline + self.params.dispute_grace,
            DEADLINE_NOT_PASSED
        );
        let bond = job.dispute_bond.0;
        let to_worker = bond / 2;
        let to_client = bond - to_worker;
        self.totals.dispute_bonds -= bond;
        self.credit(job.worker, to_worker);
        self.credit(job.client, to_client);
        if to_worker > 0 {
            events::dispute_bond_settled(job_id, job.worker, to_worker);
        }
        if to_client > 0 {
            events::dispute_bond_settled(job_id, job.client, to_client);
        }
        events::dispute_expired(job_id, to_worker, to_client);
        self.settle(&mut job, Outcome::Unresolved, false);
        self.jobs.insert(job_id, job);
    }

    /// Small, instant payment with no escrow window, for trusted workers.
    pub fn direct_pay(
        &mut self,
        client_id: AgentId,
        worker_id: AgentId,
        amount: U128,
        spec_hash: String,
    ) -> JobId {
        require!(!self.paused, PAUSED);
        let amount = amount.0;
        self.validate_hire(client_id, worker_id, amount);
        require!(
            amount <= self.params.fast_path_max.0,
            TOO_LARGE_FOR_FAST_PATH
        );
        check_hash(&spec_hash);

        let before = env::storage_usage();
        self.spend(client_id, amount);
        let fee = self.fee_of(amount);
        self.credit(worker_id, amount - fee);
        self.totals.fees_owed += fee;

        let now = now_s();
        let id = self.next_job_id;
        self.next_job_id += 1;
        let job = Job {
            id,
            client: client_id,
            worker: worker_id,
            amount: U128(amount),
            fee: U128(fee),
            state: JobState::Settled,
            accept_deadline: now,
            work_deadline: now,
            review_deadline: now,
            dispute_deadline: 0,
            review_window: 0,
            dispute_timeout: 0,
            spec_hash: spec_hash.clone(),
            result_hash: None,
            worker_bond: U128(0),
            dispute_bond: U128(0),
            bond_locked: false,
            review_hash: None,
            outcome: Some(Outcome::Success),
            created_at: now,
        };
        self.bump_worker(worker_id, |r| r.completed += 1);
        self.bump_client(client_id, |r| r.paid += 1);
        events::direct_paid(id, client_id, worker_id, amount, fee, &spec_hash);
        events::job_settled(id, worker_id, amount - fee, fee, Outcome::Success);
        self.review_worker(id, worker_id, 100);
        self.review_client(id, client_id, 100);
        self.jobs.insert(id, job);
        self.charge_storage(client_id, before, 0);
        id
    }

    // ═══════════════════════════ views ═══════════════════════════

    /// Layout version of the stored state (`STATE_VERSION`).
    pub fn version(&self) -> u8 {
        self.version
    }

    pub fn get_params(&self) -> Params {
        self.params.clone()
    }

    pub fn get_roles(&self) -> Roles {
        Roles {
            owner: self.owner.clone(),
            pending_owner: self.pending_owner.clone(),
            arbiter: self.arbiter.clone(),
            pauser: self.pauser.clone(),
            fee_recipient: self.fee_recipient.clone(),
            token: self.token.clone(),
            paused: self.paused,
        }
    }

    pub fn get_agent(&self, agent_id: AgentId) -> Option<Agent> {
        self.agents.get(&agent_id).cloned()
    }

    pub fn agent_of_wallet(&self, wallet: AccountId) -> Option<AgentId> {
        self.agent_by_wallet.get(&wallet).copied()
    }

    pub fn agent_count(&self) -> u64 {
        self.next_agent_id - 1
    }

    pub fn get_job(&self, job_id: JobId) -> Option<Job> {
        self.jobs.get(&job_id).cloned()
    }

    pub fn job_count(&self) -> u64 {
        self.next_job_id - 1
    }

    pub fn worker_record(&self, agent_id: AgentId) -> Option<WorkerRecord> {
        self.worker_records.get(&agent_id).cloned()
    }

    pub fn client_record(&self, agent_id: AgentId) -> Option<ClientRecord> {
        self.client_records.get(&agent_id).cloned()
    }

    pub fn purse_of(&self, agent_id: AgentId) -> Option<PurseView> {
        let p = self.purses.get(&agent_id)?;
        let now = now_s();
        Some(PurseView {
            balance: U128(p.balance),
            per_task_cap: U128(p.per_task_cap),
            daily_cap: U128(p.daily_cap),
            spent_today: U128(budget::window_spent(p, now)),
            daily_remaining: U128(budget::daily_remaining(p, now)),
            next_release_at: budget::next_release_at(p, now),
        })
    }

    pub fn stake_of(&self, agent_id: AgentId) -> Option<StakeView> {
        let s = self.stakes.get(&agent_id)?;
        Some(StakeView {
            available: U128(s.available),
            pending: U128(s.pending),
            unlock_at: s.unlock_at,
            locked: U128(s.locked),
            unlocked: U128(s.unlocked()),
            hireable: s.unlocked() >= self.params.min_stake.0,
        })
    }

    pub fn is_hireable(&self, agent_id: AgentId) -> bool {
        match (self.agents.get(&agent_id), self.stakes.get(&agent_id)) {
            (Some(a), Some(s)) => {
                a.wallet.is_some() && !a.frozen && s.unlocked() >= self.params.min_stake.0
            }
            _ => false,
        }
    }

    pub fn storage_balance_of(&self, agent_id: AgentId) -> U128 {
        U128(self.storage_balance(agent_id))
    }

    pub fn get_totals(&self) -> TotalsView {
        let t = &self.totals;
        TotalsView {
            locked: U128(t.locked),
            dispute_bonds: U128(t.dispute_bonds),
            balances: U128(t.balances),
            stakes: U128(t.stakes),
            fees_owed: U128(t.fees_owed),
            in_flight: U128(t.in_flight),
            sum: U128(t.sum()),
        }
    }

    /// Settlement verdict for validators (v3 SettlementValidator):
    /// 100 = settled with no dispute, 0 = dispute upheld, None = not
    /// attributable (open, cancelled, expired, or no result delivered).
    pub fn verdict_of(&self, job_id: JobId) -> Option<u8> {
        let job = self.jobs.get(&job_id)?;
        job.result_hash.as_ref()?;
        match job.state {
            JobState::Settled if job.dispute_deadline == 0 => Some(100),
            JobState::Refunded if job.dispute_deadline != 0 => Some(0),
            _ => None,
        }
    }
}

// ═══════════════════════════ tokens in ═══════════════════════════

#[near]
impl FungibleTokenReceiver for Contract {
    /// `msg` is `{"Fund":{"agent_id":N}}` or `{"Stake":{"agent_id":N}}`.
    /// A panic here makes the token contract refund the sender in full
    /// (NEP-141 `ft_resolve_transfer`), so a bad message loses nothing.
    fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        require!(env::predecessor_account_id() == self.token, WRONG_TOKEN);
        require!(amount.0 > 0, ZERO_AMOUNT);
        let parsed: TokenMsg =
            near_sdk::serde_json::from_str(&msg).unwrap_or_else(|_| env::panic_str(BAD_MSG));
        match parsed {
            TokenMsg::Fund { agent_id } => {
                self.agent(agent_id);
                self.credit(agent_id, amount.0);
                events::funded(agent_id, &sender_id, amount.0, "balance");
            }
            TokenMsg::Stake { agent_id } => {
                self.agent(agent_id);
                let mut s = self.stake(agent_id);
                s.available += amount.0;
                self.stakes.insert(agent_id, s);
                self.totals.stakes += amount.0;
                events::funded(agent_id, &sender_id, amount.0, "stake");
            }
        }
        PromiseOrValue::Value(U128(0))
    }
}

// ═══════════════════════════ internals ═══════════════════════════

impl Contract {
    fn only_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, NOT_OWNER);
    }

    fn agent(&self, id: AgentId) -> Agent {
        self.agents
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(NO_SUCH_AGENT))
    }

    fn owned_agent(&self, id: AgentId) -> Agent {
        let a = self.agent(id);
        require!(env::predecessor_account_id() == a.owner, NOT_AGENT_OWNER);
        a
    }

    /// The caller must be the agent's accepted wallet, and the agent not frozen.
    fn only_wallet_of(&self, id: AgentId) -> Agent {
        let a = self.agent(id);
        require!(
            a.wallet.as_ref() == Some(&env::predecessor_account_id()),
            NOT_AGENT_WALLET
        );
        require!(!a.frozen, AGENT_FROZEN);
        a
    }

    fn job(&self, id: JobId) -> Job {
        self.jobs
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(NO_SUCH_JOB))
    }

    fn purse(&self, id: AgentId) -> Purse {
        self.purses
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(NO_SUCH_AGENT))
    }

    fn stake(&self, id: AgentId) -> Stake {
        self.stakes
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(NO_SUCH_AGENT))
    }

    fn storage_balance(&self, id: AgentId) -> u128 {
        self.storage_balances.get(&id).copied().unwrap_or(0)
    }

    /// The v3 hire checks, in the v3 order (`TaskEscrow._validateHire`).
    /// The operator guard and the unwritten-review guard have no NEAR
    /// counterpart: reputation is written in the same call, and no other
    /// contract can act for an agent here.
    fn validate_hire(&self, client_id: AgentId, worker_id: AgentId, amount: u128) {
        require!(amount > 0, ZERO_AMOUNT);
        require!(amount >= self.params.min_job_amount.0, AMOUNT_BELOW_MINIMUM);
        require!(client_id != worker_id, SELF_DEALING);
        let client = self.only_wallet_of(client_id);
        let worker = self.agent(worker_id);
        let worker_wallet = match (&worker.wallet, worker.frozen) {
            (Some(w), false) => w.clone(),
            _ => env::panic_str(NOT_HIREABLE),
        };
        require!(worker_wallet != env::predecessor_account_id(), SELF_DEALING);
        require!(client.owner != worker.owner, SAME_OWNER);
        require!(
            self.stake(worker_id).unlocked() >= self.params.min_stake.0,
            NOT_HIREABLE
        );
    }

    /// Take `amount` from the agent's balance under its caps.
    fn spend(&mut self, id: AgentId, amount: u128) {
        let mut p = self.purse(id);
        require!(p.balance >= amount, INSUFFICIENT_BALANCE);
        budget::charge(&mut p, amount, now_s());
        p.balance -= amount;
        self.purses.insert(id, p);
        self.totals.balances -= amount;
    }

    /// Take `amount` from the agent's balance, outside its caps: for the
    /// dispute bond only, which is held and returned rather than spent.
    fn take(&mut self, id: AgentId, amount: u128) {
        let mut p = self.purse(id);
        require!(p.balance >= amount, INSUFFICIENT_BALANCE);
        p.balance -= amount;
        self.purses.insert(id, p);
        self.totals.balances -= amount;
    }

    fn credit(&mut self, id: AgentId, amount: u128) {
        if amount == 0 {
            return;
        }
        let mut p = self.purse(id);
        p.balance += amount;
        self.purses.insert(id, p);
        self.totals.balances += amount;
    }

    fn fee_of(&self, amount: u128) -> u128 {
        bps_of(amount, self.params.protocol_fee_bps).max(self.params.min_fee.0)
    }

    fn bond_of(&self, amount: u128) -> u128 {
        bps_of(amount, self.params.bond_bps).max(self.params.min_bond.0)
    }

    fn dispute_bond_of(&self, amount: u128) -> u128 {
        bps_of(amount, self.params.dispute_bond_bps).max(self.params.min_dispute_bond.0)
    }

    /// Pay the worker `amount - fee`, return its bond, record the outcome.
    fn settle(&mut self, job: &mut Job, outcome: Outcome, review: bool) {
        job.state = JobState::Settled;
        job.outcome = Some(outcome);
        self.totals.locked -= job.amount.0;
        self.release_bond(job);
        let paid = job.amount.0 - job.fee.0;
        self.credit(job.worker, paid);
        self.totals.fees_owed += job.fee.0;
        match outcome {
            Outcome::Unresolved => self.bump_worker(job.worker, |r| r.unresolved += 1),
            _ => self.bump_worker(job.worker, |r| r.completed += 1),
        }
        events::job_settled(job.id, job.worker, paid, job.fee.0, outcome);
        if review {
            self.review_worker(job.id, job.worker, 100);
        }
    }

    /// Return `amount` to the client. Only a FAILED delivery costs the worker
    /// its bond; every other refund releases it.
    fn refund(&mut self, job: &mut Job, reason: &str, outcome: Option<Outcome>) {
        job.state = JobState::Refunded;
        job.outcome = outcome;
        self.totals.locked -= job.amount.0;
        self.credit(job.client, job.amount.0);
        match outcome {
            Some(Outcome::Failed) => {
                self.seize_bond(job);
                self.bump_worker(job.worker, |r| r.failed += 1);
                self.review_worker(job.id, job.worker, 0);
            }
            Some(Outcome::DisputeLost) => {
                self.release_bond(job);
                self.bump_worker(job.worker, |r| r.disputes_lost += 1);
                self.review_worker(job.id, job.worker, 0);
            }
            _ => self.release_bond(job),
        }
        events::job_refunded(job.id, job.client, job.amount.0, reason, outcome);
    }

    fn release_bond(&mut self, job: &mut Job) {
        if !job.bond_locked {
            return;
        }
        let mut s = self.stake(job.worker);
        s.locked -= job.worker_bond.0;
        self.stakes.insert(job.worker, s);
        job.bond_locked = false;
    }

    fn seize_bond(&mut self, job: &mut Job) {
        if !job.bond_locked {
            return;
        }
        let bond = job.worker_bond.0;
        let mut s = self.stake(job.worker);
        let take = bond.min(s.available);
        s.available -= take;
        s.locked -= bond;
        self.stakes.insert(job.worker, s);
        job.bond_locked = false;
        self.totals.stakes -= take;
        self.credit(job.client, take);
        events::bond_seized(job.id, job.worker, job.client, take);
    }

    fn bump_worker(&mut self, id: AgentId, f: impl FnOnce(&mut WorkerRecord)) {
        let mut r = self.worker_records.get(&id).cloned().unwrap_or_default();
        f(&mut r);
        self.worker_records.insert(id, r);
    }

    fn bump_client(&mut self, id: AgentId, f: impl FnOnce(&mut ClientRecord)) {
        let mut r = self.client_records.get(&id).cloned().unwrap_or_default();
        f(&mut r);
        self.client_records.insert(id, r);
    }

    fn review_worker(&mut self, job: JobId, id: AgentId, value: u8) {
        self.bump_worker(id, |r| {
            r.reviews += 1;
            r.review_score_sum += value as u64;
        });
        events::review(job, id, "worker", value);
    }

    fn review_client(&mut self, job: JobId, id: AgentId, value: u8) {
        self.bump_client(id, |r| {
            r.reviews += 1;
            r.review_score_sum += value as u64;
        });
        events::review(job, id, "client", value);
    }

    fn rate_client(&mut self, job: &Job, value: u8) {
        self.review_client(job.id, job.client, value);
    }

    /// Start an outgoing transfer. The amount was already taken out of its
    /// bucket and is held in `in_flight` until the callback; if the transfer
    /// fails it goes back where it came from.
    fn send(&mut self, receiver: AccountId, amount: u128, refund: Refund) -> Promise {
        require_send_gas();
        self.totals.in_flight += amount;
        events::transfer_started(&receiver, amount, &refund);
        ext_ft_core::ext(self.token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(receiver.clone(), U128(amount), None)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_ON_SEND)
                    .on_send(receiver, U128(amount), refund),
            )
    }

    fn flush(&mut self) {
        self.agents.flush();
        self.agent_by_wallet.flush();
        self.purses.flush();
        self.stakes.flush();
        self.storage_balances.flush();
        self.jobs.flush();
        self.worker_records.flush();
        self.client_records.flush();
    }

    fn storage_cost_since(&self, before: u64) -> u128 {
        let used = env::storage_usage().saturating_sub(before);
        used as u128 * env::storage_byte_cost().as_yoctonear()
    }

    /// Bill the storage a call added to the agent's prepaid storage balance.
    fn charge_storage(&mut self, id: AgentId, before: u64, extra_bytes: u64) {
        self.flush();
        let cost = self.storage_cost_since(before)
            + extra_bytes as u128 * env::storage_byte_cost().as_yoctonear();
        let b = self.storage_balance(id);
        require!(b >= cost, INSUFFICIENT_STORAGE);
        self.storage_balances.insert(id, b - cost);
    }

    fn check_params(p: &Params) {
        require!(p.protocol_fee_bps <= MAX_FEE_BPS, FEE_TOO_HIGH);
        for w in [
            p.review_window,
            p.max_accept_window,
            p.max_work_window,
            p.dispute_timeout,
            p.dispute_grace,
        ] {
            require!((1..=MAX_WINDOW).contains(&w), INVALID_WINDOW);
        }
        require!(
            (1..=MAX_WITHDRAW_DELAY).contains(&p.withdraw_delay),
            INVALID_WINDOW
        );
        require!(
            p.min_job_amount.0 > 0 && p.min_fee.0 < p.min_job_amount.0,
            INVALID_FEE_FLOOR
        );
        require!(
            p.fast_path_max.0 == 0 || p.fast_path_max.0 >= p.min_job_amount.0,
            INVALID_FEE_FLOOR
        );
        require!(
            p.max_job_amount.0 >= p.min_job_amount.0 && p.max_job_amount.0 >= p.fast_path_max.0,
            INVALID_FEE_FLOOR
        );
        require!(
            p.bond_bps as u128 <= BPS && p.dispute_bond_bps as u128 <= BPS,
            INVALID_BOND
        );
        require!(p.min_stake.0 > 0, INVALID_STAKE);
        require!(
            p.default_per_task_cap.0 <= p.default_daily_cap.0,
            INVALID_CAPS
        );
    }
}

// ═══════════════════════════ state migration ═══════════════════════════

/// The layouts `migrate` reads. Only the top-level blob changes between
/// versions; the maps are stored under their own prefixes.
pub mod migration {
    use super::*;

    pub const NOT_V0: &str = "StateIsNotV0";

    /// `Params` as 1.0.0 stored them: no `dispute_grace`.
    #[derive(BorshSerialize, BorshDeserialize)]
    #[borsh(crate = "near_sdk::borsh")]
    pub struct ParamsV0 {
        pub protocol_fee_bps: u16,
        pub min_fee: U128,
        pub min_job_amount: U128,
        pub max_job_amount: U128,
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

    /// The contract state as 1.0.0 stored it: no `version` field.
    #[derive(BorshSerialize, BorshDeserialize)]
    #[borsh(crate = "near_sdk::borsh")]
    pub struct V0 {
        pub owner: AccountId,
        pub pending_owner: Option<AccountId>,
        pub arbiter: AccountId,
        pub pauser: AccountId,
        pub fee_recipient: AccountId,
        pub token: AccountId,
        pub paused: bool,
        pub params: ParamsV0,
        pub next_agent_id: AgentId,
        pub next_job_id: JobId,
        pub agents: LookupMap<AgentId, Agent>,
        pub agent_by_wallet: LookupMap<AccountId, AgentId>,
        pub purses: LookupMap<AgentId, Purse>,
        pub stakes: LookupMap<AgentId, Stake>,
        pub storage_balances: LookupMap<AgentId, u128>,
        pub jobs: LookupMap<JobId, Job>,
        pub worker_records: LookupMap<AgentId, WorkerRecord>,
        pub client_records: LookupMap<AgentId, ClientRecord>,
        pub totals: Totals,
    }
}
