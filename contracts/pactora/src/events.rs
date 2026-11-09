//! NEP-297 events: one `EVENT_JSON:` log line per event, standard "pactora".
//! The indexer reads these; nothing else in the contract depends on them.

use near_sdk::json_types::U128;
use near_sdk::serde_json::{json, Value};
use near_sdk::{env, AccountId};

use crate::types::{AgentId, JobId, Outcome, Refund};

pub const STANDARD: &str = "pactora";
pub const VERSION: &str = "1.0.0";

pub fn emit(event: &str, data: Value) {
    let log = json!({
        "standard": STANDARD,
        "version": VERSION,
        "event": event,
        "data": [data],
    });
    env::log_str(&format!("EVENT_JSON:{log}"));
}

pub fn agent_registered(id: AgentId, owner: &AccountId, wallet: &AccountId, card_uri: &str) {
    emit(
        "agent_registered",
        json!({ "agent_id": id, "owner": owner, "pending_wallet": wallet, "card_uri": card_uri }),
    );
}

pub fn wallet_accepted(id: AgentId, wallet: &AccountId, previous: Option<&AccountId>) {
    emit(
        "wallet_accepted",
        json!({ "agent_id": id, "wallet": wallet, "previous": previous }),
    );
}

pub fn wallet_proposed(id: AgentId, wallet: Option<&AccountId>) {
    emit(
        "wallet_proposed",
        json!({ "agent_id": id, "pending_wallet": wallet }),
    );
}

pub fn agent_frozen(id: AgentId, frozen: bool) {
    emit("agent_frozen", json!({ "agent_id": id, "frozen": frozen }));
}

pub fn caps_set(id: AgentId, per_task: u128, daily: u128) {
    emit(
        "caps_set",
        json!({ "agent_id": id, "per_task_cap": U128(per_task), "daily_cap": U128(daily) }),
    );
}

pub fn funded(id: AgentId, from: &AccountId, amount: u128, kind: &str) {
    emit(
        "funded",
        json!({ "agent_id": id, "from": from, "amount": U128(amount), "kind": kind }),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn job_created(
    id: JobId,
    client: AgentId,
    worker: AgentId,
    amount: u128,
    fee: u128,
    spec_hash: &str,
    accept_deadline: u64,
    work_deadline: u64,
) {
    emit(
        "job_created",
        json!({
            "job_id": id, "client": client, "worker": worker,
            "amount": U128(amount), "fee": U128(fee), "spec_hash": spec_hash,
            "accept_deadline": accept_deadline, "work_deadline": work_deadline,
        }),
    );
}

pub fn job_accepted(id: JobId, worker: AgentId, bond: u128, work_deadline: u64) {
    emit(
        "job_accepted",
        json!({ "job_id": id, "worker": worker, "bond": U128(bond), "work_deadline": work_deadline }),
    );
}

pub fn result_submitted(id: JobId, result_hash: &str, review_deadline: u64) {
    emit(
        "result_submitted",
        json!({ "job_id": id, "result_hash": result_hash, "review_deadline": review_deadline }),
    );
}

pub fn job_disputed(id: JobId, bond: u128, dispute_deadline: u64, reason_hash: Option<&str>) {
    emit(
        "job_disputed",
        json!({
            "job_id": id, "dispute_bond": U128(bond),
            "dispute_deadline": dispute_deadline, "reason_hash": reason_hash,
        }),
    );
}

pub fn dispute_resolved(id: JobId, for_worker: bool, arbiter: &AccountId) {
    emit(
        "dispute_resolved",
        json!({ "job_id": id, "for_worker": for_worker, "arbiter": arbiter }),
    );
}

/// `bond_to_worker` / `bond_to_client`: how the client's dispute bond was
/// split (added in 1.1.0; the indexer reads `job_id` and may ignore them).
pub fn dispute_expired(id: JobId, bond_to_worker: u128, bond_to_client: u128) {
    emit(
        "dispute_expired",
        json!({
            "job_id": id,
            "bond_to_worker": U128(bond_to_worker),
            "bond_to_client": U128(bond_to_client),
        }),
    );
}

pub fn job_settled(id: JobId, worker: AgentId, paid: u128, fee: u128, outcome: Outcome) {
    emit(
        "job_settled",
        json!({
            "job_id": id, "worker": worker, "paid": U128(paid), "fee": U128(fee), "outcome": outcome,
        }),
    );
}

pub fn job_refunded(
    id: JobId,
    client: AgentId,
    amount: u128,
    reason: &str,
    outcome: Option<Outcome>,
) {
    emit(
        "job_refunded",
        json!({
            "job_id": id, "client": client, "amount": U128(amount),
            "reason": reason, "outcome": outcome,
        }),
    );
}

pub fn bond_seized(id: JobId, worker: AgentId, client: AgentId, amount: u128) {
    emit(
        "bond_seized",
        json!({ "job_id": id, "worker": worker, "client": client, "amount": U128(amount) }),
    );
}

pub fn dispute_bond_settled(id: JobId, to: AgentId, amount: u128) {
    emit(
        "dispute_bond_settled",
        json!({ "job_id": id, "to": to, "amount": U128(amount) }),
    );
}

pub fn direct_paid(
    id: JobId,
    client: AgentId,
    worker: AgentId,
    amount: u128,
    fee: u128,
    spec_hash: &str,
) {
    emit(
        "direct_paid",
        json!({
            "job_id": id, "client": client, "worker": worker,
            "amount": U128(amount), "fee": U128(fee), "spec_hash": spec_hash,
        }),
    );
}

pub fn review(job: JobId, agent: AgentId, side: &str, value: u8) {
    emit(
        "review",
        json!({ "job_id": job, "agent_id": agent, "side": side, "value": value }),
    );
}

pub fn unstake_requested(id: AgentId, amount: u128, unlock_at: u64) {
    emit(
        "unstake_requested",
        json!({ "agent_id": id, "amount": U128(amount), "unlock_at": unlock_at }),
    );
}

pub fn unstake_cancelled(id: AgentId, amount: u128) {
    emit(
        "unstake_cancelled",
        json!({ "agent_id": id, "amount": U128(amount) }),
    );
}

/// The matured stake reached the owner (emitted from `on_send`, on success).
pub fn unstaked(id: AgentId, amount: u128) {
    emit(
        "unstaked",
        json!({ "agent_id": id, "amount": U128(amount) }),
    );
}

/// The payout of matured stake failed and the amount is back in the stake.
pub fn stake_restored(id: AgentId, amount: u128) {
    emit(
        "stake_restored",
        json!({ "agent_id": id, "amount": U128(amount) }),
    );
}

/// `recover_surplus`: `amount` of the `surplus` above `totals.sum` is being
/// sent to `to`.
pub fn surplus_recovered(to: &AccountId, amount: u128, surplus: u128) {
    emit(
        "surplus_recovered",
        json!({ "to": to, "amount": U128(amount), "surplus": U128(surplus) }),
    );
}

pub fn transfer_started(to: &AccountId, amount: u128, refund: &Refund) {
    emit(
        "transfer_started",
        json!({ "to": to, "amount": U128(amount), "refund_to": refund }),
    );
}

pub fn transfer_finished(to: &AccountId, amount: u128, refund: &Refund, ok: bool) {
    emit(
        "transfer_finished",
        json!({ "to": to, "amount": U128(amount), "refund_to": refund, "ok": ok }),
    );
}

pub fn paused(by: &AccountId, paused: bool) {
    emit("paused", json!({ "by": by, "paused": paused }));
}

pub fn role_set(role: &str, account: &AccountId) {
    emit("role_set", json!({ "role": role, "account": account }));
}

pub fn params_set() {
    emit("params_set", json!({}));
}
