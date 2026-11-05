//! Integration tests on a local NEAR sandbox node, against the compiled wasm
//! of both contracts. What only a real runtime can prove lives here:
//! `#[private]` callbacks, function-call keys that cannot attach deposits,
//! NEP-141 `ft_transfer_call` funding, asynchronous transfer failure, storage
//! billing, and invariant I1 against the token contract's real balance.
//!
//! Run in Linux (WSL/CI): `cargo test -p pactora --test sandbox`.

use near_workspaces::network::Sandbox;
use near_workspaces::types::{AccessKey, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};

const SPEC: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const RESULT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct Env {
    worker: Worker<Sandbox>,
    usdc: Contract,
    pactora: Contract,
    alice: Account,
    bob: Account,
    client_wallet: Account,
    worker_wallet: Account,
    dao: Account,
    fees: Account,
}

fn params() -> Value {
    json!({
        "protocol_fee_bps": 100, "min_fee": "100", "min_job_amount": "10000",
        "max_job_amount": "10000000", "fast_path_max": "30000",
        "review_window": 600, "max_accept_window": 3000, "max_work_window": 18000,
        "dispute_timeout": 3600, "bond_bps": 2000, "min_bond": "10000",
        "dispute_bond_bps": 1000, "min_dispute_bond": "10000", "min_stake": "10000000",
        "withdraw_delay": 604800, "default_per_task_cap": "5000000",
        "default_daily_cap": "20000000"
    })
}

async fn sub(root: &Account, name: &str, near: u128) -> anyhow::Result<Account> {
    Ok(root
        .create_subaccount(name)
        .initial_balance(NearToken::from_near(near))
        .transact()
        .await?
        .into_result()?)
}

async fn ft_register(usdc: &Contract, who: &Account) -> anyhow::Result<()> {
    who.call(usdc.id(), "storage_deposit")
        .args_json(json!({ "account_id": who.id() }))
        .deposit(NearToken::from_millinear(10))
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

async fn ft_balance(usdc: &Contract, id: &near_workspaces::AccountId) -> anyhow::Result<u128> {
    let v: String = usdc
        .view("ft_balance_of")
        .args_json(json!({ "account_id": id }))
        .await?
        .json()?;
    Ok(v.parse()?)
}

async fn totals_sum(p: &Contract) -> anyhow::Result<u128> {
    let t: Value = p.view("get_totals").await?.json()?;
    Ok(t["sum"].as_str().unwrap().parse()?)
}

/// I1 against the real token balance.
async fn check_i1(e: &Env) -> anyhow::Result<()> {
    let held = ft_balance(&e.usdc, e.pactora.id()).await?;
    let sum = totals_sum(&e.pactora).await?;
    assert_eq!(held, sum, "I1: token balance {held} vs totals {sum}");
    Ok(())
}

/// The wasm under test: the file `$var` names when set — CI sets it to the
/// artifact its build job produced, so these tests run the exact bytes that
/// get deployed — else built from source with cargo-near.
async fn wasm(var: &str, project: &str) -> anyhow::Result<Vec<u8>> {
    match std::env::var(var) {
        Ok(path) => Ok(std::fs::read(&path).map_err(|e| anyhow::anyhow!("{var}={path}: {e}"))?),
        Err(_) => Ok(near_workspaces::compile_project(project).await?),
    }
}

async fn setup() -> anyhow::Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let usdc_wasm = wasm("MOCK_USDC_WASM", "../mock-usdc").await?;
    let pactora_wasm = wasm("PACTORA_WASM", "./").await?;
    let root = worker.root_account()?;

    let usdc = sub(&root, "usdc", 20)
        .await?
        .deploy(&usdc_wasm)
        .await?
        .into_result()?;
    let pactora = sub(&root, "pactora", 50)
        .await?
        .deploy(&pactora_wasm)
        .await?
        .into_result()?;
    let alice = sub(&root, "alice", 50).await?;
    let bob = sub(&root, "bob", 50).await?;
    let dao = sub(&root, "dao", 10).await?;
    let fees = sub(&root, "fees", 10).await?;
    let client_wallet = sub(&alice, "c", 10).await?;
    let worker_wallet = sub(&bob, "w", 10).await?;

    usdc.call("new")
        .args_json(json!({ "owner": usdc.id() }))
        .transact()
        .await?
        .into_result()?;
    pactora
        .call("new")
        .args_json(json!({
            "owner": pactora.id(), "token": usdc.id(), "arbiter": dao.id(),
            "pauser": pactora.id(), "fee_recipient": fees.id(), "params": params(),
        }))
        .transact()
        .await?
        .into_result()?;

    for a in [&alice, &bob, &fees] {
        ft_register(&usdc, a).await?;
    }
    // The escrow itself must be registered to hold tokens.
    pactora
        .as_account()
        .call(usdc.id(), "storage_deposit")
        .args_json(json!({ "account_id": pactora.id() }))
        .deposit(NearToken::from_millinear(10))
        .transact()
        .await?
        .into_result()?;
    for a in [&alice, &bob] {
        usdc.call("mint")
            .args_json(json!({ "account_id": a.id(), "amount": "100000000" }))
            .transact()
            .await?
            .into_result()?;
    }

    Ok(Env {
        worker,
        usdc,
        pactora,
        alice,
        bob,
        client_wallet,
        worker_wallet,
        dao,
        fees,
    })
}

async fn register(e: &Env, owner: &Account, wallet: &Account) -> anyhow::Result<u64> {
    let id: u64 = owner
        .call(e.pactora.id(), "register_agent")
        .args_json(json!({ "wallet": wallet.id(), "card_uri": "ipfs://card" }))
        .deposit(NearToken::from_near(1))
        .transact()
        .await?
        .into_result()?
        .json()?;
    wallet
        .call(e.pactora.id(), "accept_wallet")
        .args_json(json!({ "agent_id": id }))
        .transact()
        .await?
        .into_result()?;
    Ok(id)
}

async fn send_in(e: &Env, from: &Account, amount: u128, msg: Value) -> anyhow::Result<()> {
    from.call(e.usdc.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": e.pactora.id(), "amount": amount.to_string(), "msg": msg.to_string(),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

/// Two hireable agents: alice's client (funded) and bob's worker (staked).
async fn market() -> anyhow::Result<(Env, u64, u64)> {
    let e = setup().await?;
    let client = register(&e, &e.alice, &e.client_wallet).await?;
    let worker = register(&e, &e.bob, &e.worker_wallet).await?;
    send_in(
        &e,
        &e.alice,
        10_000_000,
        json!({ "Fund": { "agent_id": client } }),
    )
    .await?;
    send_in(
        &e,
        &e.bob,
        20_000_000,
        json!({ "Stake": { "agent_id": worker } }),
    )
    .await?;
    Ok((e, client, worker))
}

/// An account object that signs with a fresh function-call key scoped to
/// the escrow — what an agent's runtime would hold.
async fn fc_key_account(e: &Env, owner: &Account) -> anyhow::Result<Account> {
    let sk = SecretKey::from_random(KeyType::ED25519);
    owner
        .batch(owner.id())
        .add_key(
            sk.public_key(),
            AccessKey::function_call_access(e.pactora.id(), &[], Some(NearToken::from_near(1))),
        )
        .transact()
        .await?
        .into_result()?;
    Ok(Account::from_secret_key(owner.id().clone(), sk, &e.worker))
}

#[tokio::test]
async fn funding_and_the_full_escrow_path_keep_i1() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    check_i1(&e).await?;

    let job: u64 = e
        .client_wallet
        .call(e.pactora.id(), "create_job")
        .args_json(json!({
            "client_id": client, "worker_id": worker, "amount": "1000000",
            "spec_hash": SPEC, "accept_window": 300, "work_window": 1800,
        }))
        .transact()
        .await?
        .into_result()?
        .json()?;
    for (m, args) in [
        ("accept_job", json!({ "job_id": job })),
        (
            "submit_result",
            json!({ "job_id": job, "result_hash": RESULT }),
        ),
    ] {
        e.worker_wallet
            .call(e.pactora.id(), m)
            .args_json(args)
            .transact()
            .await?
            .into_result()?;
    }
    e.client_wallet
        .call(e.pactora.id(), "approve")
        .args_json(json!({ "job_id": job }))
        .transact()
        .await?
        .into_result()?;

    let j: Value = e
        .pactora
        .view("get_job")
        .args_json(json!({ "job_id": job }))
        .await?
        .json()?;
    assert_eq!(j["state"], "Settled");
    check_i1(&e).await?;

    // Bob withdraws the worker's earnings to his own account.
    let before = ft_balance(&e.usdc, e.bob.id()).await?;
    e.bob
        .call(e.pactora.id(), "withdraw")
        .args_json(json!({ "agent_id": worker, "amount": "990000" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    assert_eq!(ft_balance(&e.usdc, e.bob.id()).await? - before, 990_000);
    check_i1(&e).await?;
    Ok(())
}

#[tokio::test]
async fn a_failed_transfer_puts_the_money_back() -> anyhow::Result<()> {
    let (e, client, _) = market().await?;
    let stranger = sub(&e.worker.root_account()?, "stranger", 5).await?; // not registered on the token
    let before: Value = e
        .pactora
        .view("purse_of")
        .args_json(json!({ "agent_id": client }))
        .await?
        .json()?;
    let r = e
        .alice
        .call(e.pactora.id(), "withdraw")
        .args_json(json!({ "agent_id": client, "amount": "1000000", "receiver": stranger.id() }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;
    // The call itself succeeds; the inner ft_transfer fails and on_send restores.
    let ok: bool = r.json()?;
    assert!(!ok, "the transfer to an unregistered account must fail");
    let after: Value = e
        .pactora
        .view("purse_of")
        .args_json(json!({ "agent_id": client }))
        .await?
        .json()?;
    assert_eq!(before["balance"], after["balance"]);
    let t: Value = e.pactora.view("get_totals").await?.json()?;
    assert_eq!(t["in_flight"], "0");
    check_i1(&e).await?;
    Ok(())
}

#[tokio::test]
async fn the_send_callback_is_private() -> anyhow::Result<()> {
    let (e, client, _) = market().await?;
    let r = e
        .alice
        .call(e.pactora.id(), "on_send")
        .args_json(
            json!({ "receiver": e.alice.id(), "amount": "1", "refund": { "Purse": client } }),
        )
        .transact()
        .await?;
    assert!(r.is_failure());
    assert!(format!("{:?}", r.into_result().unwrap_err()).contains("Method on_send is private"));
    Ok(())
}

#[tokio::test]
async fn a_function_call_key_can_hire_but_never_take_money_out() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let agent_key = fc_key_account(&e, &e.client_wallet).await?;

    // Hiring works with the scoped key: no deposit needed.
    agent_key
        .call(e.pactora.id(), "create_job")
        .args_json(json!({
            "client_id": client, "worker_id": worker, "amount": "100000",
            "spec_hash": SPEC, "accept_window": 300, "work_window": 1800,
        }))
        .transact()
        .await?
        .into_result()?;

    // The same key cannot attach the yocto that `withdraw` requires...
    let r = agent_key
        .call(e.pactora.id(), "withdraw")
        .args_json(json!({ "agent_id": client, "amount": "1" }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await;
    assert!(
        r.is_err() || r.unwrap().is_failure(),
        "a function-call key must not attach a deposit"
    );

    // ...nor call the token contract directly.
    let r = agent_key
        .call(e.usdc.id(), "ft_transfer")
        .args_json(json!({ "receiver_id": e.alice.id(), "amount": "1" }))
        .transact()
        .await;
    assert!(
        r.is_err() || r.unwrap().is_failure(),
        "the key is scoped to the escrow only"
    );
    check_i1(&e).await?;
    Ok(())
}

#[tokio::test]
async fn a_bad_funding_message_is_refunded_by_the_token() -> anyhow::Result<()> {
    let (e, _, _) = market().await?;
    let before = ft_balance(&e.usdc, e.alice.id()).await?;
    e.alice
        .call(e.usdc.id(), "ft_transfer_call")
        .args_json(json!({ "receiver_id": e.pactora.id(), "amount": "5000", "msg": "nonsense" }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    assert_eq!(
        ft_balance(&e.usdc, e.alice.id()).await?,
        before,
        "NEP-141 refunds the sender"
    );
    check_i1(&e).await?;
    Ok(())
}

#[tokio::test]
async fn the_arbiter_rules_and_fees_reach_the_recipient() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let job: u64 = e
        .client_wallet
        .call(e.pactora.id(), "create_job")
        .args_json(json!({
            "client_id": client, "worker_id": worker, "amount": "1000000",
            "spec_hash": SPEC, "accept_window": 300, "work_window": 1800,
        }))
        .transact()
        .await?
        .into_result()?
        .json()?;
    e.worker_wallet
        .call(e.pactora.id(), "accept_job")
        .args_json(json!({ "job_id": job }))
        .transact()
        .await?
        .into_result()?;
    e.worker_wallet
        .call(e.pactora.id(), "submit_result")
        .args_json(json!({ "job_id": job, "result_hash": RESULT }))
        .transact()
        .await?
        .into_result()?;
    e.client_wallet
        .call(e.pactora.id(), "dispute")
        .args_json(json!({ "job_id": job }))
        .transact()
        .await?
        .into_result()?;

    let r = e
        .alice
        .call(e.pactora.id(), "resolve_dispute")
        .args_json(json!({ "job_id": job, "for_worker": false }))
        .transact()
        .await?;
    assert!(r.is_failure(), "only the arbiter rules");
    e.dao
        .call(e.pactora.id(), "resolve_dispute")
        .args_json(json!({ "job_id": job, "for_worker": true }))
        .transact()
        .await?
        .into_result()?;
    let v: Option<u8> = e
        .pactora
        .view("verdict_of")
        .args_json(json!({ "job_id": job }))
        .await?
        .json()?;
    assert_eq!(v, None, "a disputed settlement is not attributable");
    check_i1(&e).await?;

    e.alice
        .call(e.pactora.id(), "claim_fees")
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    assert_eq!(ft_balance(&e.usdc, e.fees.id()).await?, 10_000);
    check_i1(&e).await?;
    Ok(())
}
