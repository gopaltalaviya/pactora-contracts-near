//! Adversarial integration tests on a local NEAR sandbox: attacks that only
//! a real runtime can show — promises and their callbacks, racing
//! transactions, prepaid gas, storage staking, a real Sputnik DAO arbiter,
//! a code upgrade over live state, and NEP-297 events as the indexer reads
//! them.
//!
//! Run on a Linux with glibc 2.38+ (CI, or Docker): `cargo test -p pactora
//! --test adversarial`. `PACTORA_WASM` / `MOCK_USDC_WASM` name the wasm under
//! test (else built with cargo-near). `OLD_PACTORA_WASM` (the wasm of the
//! previous release, `wasm/previous/pactora.wasm`) and `SPUTNIK_DAO_WASM`
//! (scripts/create-dao.mjs --fetch-wasm) enable the upgrade and DAO tests;
//! without them those tests print a note and pass.
//!
//! Written by the 2026-11-09 audit; the ruling-vs-expiry race, the
//! `unstaked` event and the bond split on expiry were run against the code
//! before the fixes and failed.

#![allow(unused_must_use)]

use near_workspaces::network::Sandbox;
use near_workspaces::operations::Function;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const SPEC: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const RESULT: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const REASON: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const BYTE_COST: u128 = 10_000_000_000_000_000_000; // 1e19 yocto per byte

fn tgas(n: u64) -> Gas {
    Gas::from_tgas(n)
}
fn yocto(n: u128) -> NearToken {
    NearToken::from_yoctonear(n)
}

/// Windows short enough to pass with `fast_forward`.
fn params() -> Value {
    json!({
        "protocol_fee_bps": 100, "min_fee": "100", "min_job_amount": "10000",
        "max_job_amount": "10000000", "fast_path_max": "30000",
        "review_window": 20, "max_accept_window": 3000, "max_work_window": 18000,
        "dispute_timeout": 20, "dispute_grace": 20, "bond_bps": 2000, "min_bond": "10000",
        "dispute_bond_bps": 1000, "min_dispute_bond": "10000", "min_stake": "10000000",
        "withdraw_delay": 20, "default_per_task_cap": "5000000",
        "default_daily_cap": "50000000"
    })
}

struct Env {
    worker: Worker<Sandbox>,
    root: Account,
    usdc: Contract,
    pactora: Contract,
    alice: Account,
    bob: Account,
    cw: Account,
    ww: Account,
    dao: Account,
    fees: Account,
}

async fn sub(root: &Account, name: &str, near: u128) -> anyhow::Result<Account> {
    Ok(root
        .create_subaccount(name)
        .initial_balance(NearToken::from_near(near))
        .transact()
        .await?
        .into_result()?)
}

async fn wasm_from(var: &str, project: &str) -> anyhow::Result<Vec<u8>> {
    match std::env::var(var) {
        Ok(path) => Ok(std::fs::read(&path).map_err(|e| anyhow::anyhow!("{var}={path}: {e}"))?),
        Err(_) => Ok(near_workspaces::compile_project(project).await?),
    }
}

fn optional_wasm(var: &str) -> Option<Vec<u8>> {
    std::env::var(var).ok().and_then(|p| std::fs::read(p).ok())
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

async fn setup_with(pactora_wasm: &[u8]) -> anyhow::Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let usdc_wasm = wasm_from("MOCK_USDC_WASM", "../mock-usdc").await?;
    let root = worker.root_account()?;
    let usdc = sub(&root, "usdc", 20)
        .await?
        .deploy(&usdc_wasm)
        .await?
        .into_result()?;
    let pactora = sub(&root, "pactora", 50)
        .await?
        .deploy(pactora_wasm)
        .await?
        .into_result()?;
    let alice = sub(&root, "alice", 100).await?;
    let bob = sub(&root, "bob", 100).await?;
    let dao = sub(&root, "dao", 10).await?;
    let fees = sub(&root, "fees", 10).await?;
    let cw = sub(&alice, "c", 10).await?;
    let ww = sub(&bob, "w", 10).await?;
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
    for a in [&alice, &bob, &fees, pactora.as_account()] {
        ft_register(&usdc, a).await?;
    }
    for a in [&alice, &bob] {
        usdc.call("mint")
            .args_json(json!({ "account_id": a.id(), "amount": "100000000" }))
            .transact()
            .await?
            .into_result()?;
    }
    Ok(Env {
        worker,
        root,
        usdc,
        pactora,
        alice,
        bob,
        cw,
        ww,
        dao,
        fees,
    })
}

async fn setup() -> anyhow::Result<Env> {
    let w = wasm_from("PACTORA_WASM", "./").await?;
    setup_with(&w).await
}

// ───────────────────────────── call helpers ─────────────────────────────

async fn call(
    who: &Account,
    c: &AccountId,
    m: &str,
    args: Value,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who
        .call(c, m)
        .args_json(args)
        .gas(tgas(100))
        .transact()
        .await?)
}

async fn call1(
    who: &Account,
    c: &AccountId,
    m: &str,
    args: Value,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who
        .call(c, m)
        .args_json(args)
        .deposit(yocto(1))
        .gas(tgas(100))
        .transact()
        .await?)
}

fn ok(r: ExecutionFinalResult, what: &str) -> ExecutionFinalResult {
    assert!(r.is_success(), "{what} failed: {r:?}");
    r
}

/// The call failed and the failure names `msg`.
fn refused(r: &ExecutionFinalResult, msg: &str, what: &str) {
    assert!(r.is_failure(), "{what} must fail ({msg}) but succeeded");
    let s = format!("{r:?}");
    assert!(s.contains(msg), "{what}: expected `{msg}`, got {s}");
}

async fn view(e: &Env, m: &str, args: Value) -> anyhow::Result<Value> {
    Ok(e.pactora.view(m).args_json(args).await?.json()?)
}

fn n(v: &Value) -> u128 {
    match v {
        Value::String(s) => s.parse().unwrap(),
        Value::Number(x) => x.as_u64().unwrap() as u128,
        _ => panic!("not a number: {v}"),
    }
}

async fn ft_balance(e: &Env, id: &AccountId) -> anyhow::Result<u128> {
    let v: String = e
        .usdc
        .view("ft_balance_of")
        .args_json(json!({ "account_id": id }))
        .await?
        .json()?;
    Ok(v.parse()?)
}

async fn totals(e: &Env) -> anyhow::Result<Value> {
    view(e, "get_totals", json!({})).await
}

/// Invariant I1 against the real token balance, and nothing in flight.
async fn check_i1(e: &Env) -> anyhow::Result<()> {
    let held = ft_balance(e, e.pactora.id()).await?;
    let t = totals(e).await?;
    assert_eq!(held, n(&t["sum"]), "I1: token balance {held} vs totals {t}");
    assert_eq!(t["in_flight"], "0", "nothing may stay in flight: {t}");
    Ok(())
}

async fn balance_of(e: &Env, agent: u64) -> anyhow::Result<u128> {
    Ok(n(
        &view(e, "purse_of", json!({ "agent_id": agent })).await?["balance"]
    ))
}

async fn stake_of(e: &Env, agent: u64) -> anyhow::Result<Value> {
    view(e, "stake_of", json!({ "agent_id": agent })).await
}

async fn job(e: &Env, id: u64) -> anyhow::Result<Value> {
    view(e, "get_job", json!({ "job_id": id })).await
}

async fn storage_bal(e: &Env, agent: u64) -> anyhow::Result<u128> {
    Ok(n(&view(
        e,
        "storage_balance_of",
        json!({ "agent_id": agent }),
    )
    .await?))
}

async fn near_of(e: &Env, id: &AccountId) -> anyhow::Result<u128> {
    Ok(e.worker.view_account(id).await?.balance.as_yoctonear())
}

/// The contract's NEAR not locked for storage, less every agent's prepaid
/// storage balance: what the contract owns outright. No call by anyone may
/// lower it (gas rewards and 1-yocto deposits only raise it).
async fn contract_own_near(e: &Env) -> anyhow::Result<i128> {
    let a = e.worker.view_account(e.pactora.id()).await?;
    let free = a.balance.as_yoctonear() as i128 - (a.storage_usage as u128 * BYTE_COST) as i128;
    let count = n(&view(e, "agent_count", json!({})).await?) as u64;
    let mut prepaid = 0i128;
    for id in 1..=count {
        prepaid += storage_bal(e, id).await? as i128;
    }
    Ok(free - prepaid)
}

/// `dispute_grace` from the live params.
async fn grace(e: &Env) -> anyhow::Result<u64> {
    Ok(n(&view(e, "get_params", json!({})).await?["dispute_grace"]) as u64)
}

async fn now_s(e: &Env) -> anyhow::Result<u64> {
    Ok(e.worker.view_block().await?.timestamp() / 1_000_000_000)
}

async fn wait_past(e: &Env, t: u64) -> anyhow::Result<()> {
    while now_s(e).await? <= t + 1 {
        e.worker.fast_forward(20).await?;
    }
    Ok(())
}

// ───────────────────────────── event helpers ─────────────────────────────

/// Events in the SUCCESSFUL receipts executed by the contract — what the
/// indexer (`eventsOf`, which skips `!r.ok`) would store.
fn events(r: &ExecutionFinalResult, pactora: &AccountId) -> Vec<Value> {
    r.receipt_outcomes()
        .iter()
        .filter(|o| &o.executor_id == pactora && o.is_success())
        .flat_map(|o| o.logs.iter())
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .map(|s| serde_json::from_str(s).expect("EVENT_JSON is JSON"))
        .collect()
}

fn names(evs: &[Value]) -> Vec<String> {
    evs.iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect()
}

/// Logs left in FAILED receipts (dropped by the indexer).
fn failed_logs(r: &ExecutionFinalResult) -> Vec<String> {
    r.receipt_outcomes()
        .iter()
        .filter(|o| o.is_failure())
        .flat_map(|o| o.logs.iter().cloned())
        .collect()
}

/// The fields `apps/indexer/src/events.ts` JOB_EVENTS requires, with types.
fn indexer_schema(event: &str) -> Option<&'static [(&'static str, &'static str)]> {
    Some(match event {
        "job_created" => &[
            ("job_id", "u64"),
            ("client", "u64"),
            ("worker", "u64"),
            ("amount", "u128"),
            ("fee", "u128"),
            ("spec_hash", "hash"),
            ("accept_deadline", "u64"),
            ("work_deadline", "u64"),
        ],
        "job_accepted" => &[
            ("job_id", "u64"),
            ("worker", "u64"),
            ("bond", "u128"),
            ("work_deadline", "u64"),
        ],
        "result_submitted" => &[
            ("job_id", "u64"),
            ("result_hash", "hash"),
            ("review_deadline", "u64"),
        ],
        "job_disputed" => &[
            ("job_id", "u64"),
            ("dispute_bond", "u128"),
            ("dispute_deadline", "u64"),
            ("reason_hash", "hash?"),
        ],
        "dispute_resolved" => &[
            ("job_id", "u64"),
            ("for_worker", "bool"),
            ("arbiter", "string"),
        ],
        "dispute_expired" => &[
            ("job_id", "u64"),
            ("bond_to_worker", "u128"),
            ("bond_to_client", "u128"),
        ],
        "job_settled" => &[
            ("job_id", "u64"),
            ("worker", "u64"),
            ("paid", "u128"),
            ("fee", "u128"),
            ("outcome", "outcome"),
        ],
        "job_refunded" => &[
            ("job_id", "u64"),
            ("client", "u64"),
            ("amount", "u128"),
            ("reason", "string"),
            ("outcome", "outcome?"),
        ],
        "bond_seized" => &[
            ("job_id", "u64"),
            ("worker", "u64"),
            ("client", "u64"),
            ("amount", "u128"),
        ],
        "dispute_bond_settled" => &[("job_id", "u64"), ("to", "u64"), ("amount", "u128")],
        "direct_paid" => &[
            ("job_id", "u64"),
            ("client", "u64"),
            ("worker", "u64"),
            ("amount", "u128"),
            ("fee", "u128"),
            ("spec_hash", "hash"),
        ],
        _ => return None,
    })
}

fn is_hash(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
fn is_dec(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
    })
}
fn is_outcome(v: &Value) -> bool {
    matches!(
        v.as_str(),
        Some("Success" | "Failed" | "DisputeLost" | "Unresolved")
    )
}

/// The event decodes exactly as the indexer's decodePactoraEvent would.
fn check_indexer(ev: &Value) {
    assert_eq!(ev["standard"], "pactora", "{ev}");
    assert_eq!(ev["version"], "1.0.0", "{ev}");
    let data = ev["data"].as_array().expect("data array");
    assert_eq!(data.len(), 1, "data must hold exactly one object: {ev}");
    let d = data[0].as_object().expect("data[0] object");
    let name = ev["event"].as_str().unwrap();
    if name == "funded" {
        assert!(
            d["agent_id"].is_u64() && d["from"].is_string() && is_dec(&d["amount"]),
            "{ev}"
        );
        return;
    }
    let Some(schema) = indexer_schema(name) else {
        return;
    };
    for (f, ty) in schema {
        let v = d
            .get(*f)
            .unwrap_or_else(|| panic!("{name}: field {f} missing: {ev}"));
        let good = match *ty {
            "u64" => v.as_u64().is_some_and(|x| x < (1u64 << 53)),
            "u128" => is_dec(v),
            "hash" => is_hash(v),
            "hash?" => v.is_null() || is_hash(v),
            "string" => v.is_string(),
            "bool" => v.is_boolean(),
            "outcome" => is_outcome(v),
            "outcome?" => v.is_null() || is_outcome(v),
            _ => unreachable!(),
        };
        assert!(good, "{name}: field {f} is not {ty}: {ev}");
    }
    // No extra keys the indexer silently drops.
    assert_eq!(
        d.len(),
        schema.len(),
        "{name}: unexpected extra fields: {ev}"
    );
}

fn field<'a>(evs: &'a [Value], name: &str) -> &'a Value {
    &evs.iter()
        .find(|e| e["event"] == name)
        .unwrap_or_else(|| panic!("no {name} in {evs:?}"))["data"][0]
}

// ───────────────────────────── protocol helpers ─────────────────────────────

async fn register(e: &Env, owner: &Account, wallet: &Account, near: u128) -> anyhow::Result<u64> {
    let id: u64 = owner
        .call(e.pactora.id(), "register_agent")
        .args_json(json!({ "wallet": wallet.id(), "card_uri": "ipfs://card" }))
        .deposit(NearToken::from_near(near))
        .transact()
        .await?
        .into_result()?
        .json()?;
    ok(
        call(
            wallet,
            e.pactora.id(),
            "accept_wallet",
            json!({ "agent_id": id }),
        )
        .await?,
        "accept_wallet",
    );
    Ok(id)
}

async fn send_in(
    e: &Env,
    from: &Account,
    amount: u128,
    msg: Value,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(ok(
        from.call(e.usdc.id(), "ft_transfer_call")
            .args_json(json!({
                "receiver_id": e.pactora.id(), "amount": amount.to_string(), "msg": msg.to_string(),
            }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "ft_transfer_call",
    ))
}

/// Alice's client (funded 20M) and Bob's worker (staked 20M).
async fn market_on(e: &Env) -> anyhow::Result<(u64, u64)> {
    let client = register(e, &e.alice, &e.cw, 1).await?;
    let worker = register(e, &e.bob, &e.ww, 1).await?;
    send_in(
        e,
        &e.alice,
        20_000_000,
        json!({ "Fund": { "agent_id": client } }),
    )
    .await?;
    send_in(
        e,
        &e.bob,
        20_000_000,
        json!({ "Stake": { "agent_id": worker } }),
    )
    .await?;
    Ok((client, worker))
}

async fn market() -> anyhow::Result<(Env, u64, u64)> {
    let e = setup().await?;
    let (c, w) = market_on(&e).await?;
    Ok((e, c, w))
}

async fn create_job(
    e: &Env,
    c: u64,
    w: u64,
    amount: u128,
    aw: u64,
    ww: u64,
) -> anyhow::Result<u64> {
    let r = ok(
        call(
            &e.cw,
            e.pactora.id(),
            "create_job",
            json!({
                "client_id": c, "worker_id": w, "amount": amount.to_string(),
                "spec_hash": SPEC, "accept_window": aw, "work_window": ww,
            }),
        )
        .await?,
        "create_job",
    );
    Ok(r.json()?)
}

async fn accept(e: &Env, j: u64) -> anyhow::Result<ExecutionFinalResult> {
    Ok(ok(
        call(&e.ww, e.pactora.id(), "accept_job", json!({ "job_id": j })).await?,
        "accept_job",
    ))
}
async fn submit(e: &Env, j: u64) -> anyhow::Result<ExecutionFinalResult> {
    Ok(ok(
        call(
            &e.ww,
            e.pactora.id(),
            "submit_result",
            json!({ "job_id": j, "result_hash": RESULT }),
        )
        .await?,
        "submit_result",
    ))
}
async fn dispute(e: &Env, j: u64) -> anyhow::Result<ExecutionFinalResult> {
    Ok(ok(
        call(
            &e.cw,
            e.pactora.id(),
            "dispute",
            json!({ "job_id": j, "reason_hash": REASON }),
        )
        .await?,
        "dispute",
    ))
}
async fn submitted_job(e: &Env, c: u64, w: u64, amount: u128) -> anyhow::Result<u64> {
    let j = create_job(e, c, w, amount, 300, 1800).await?;
    accept(e, j).await?;
    submit(e, j).await?;
    Ok(j)
}
async fn disputed_job(e: &Env, c: u64, w: u64, amount: u128) -> anyhow::Result<u64> {
    let j = submitted_job(e, c, w, amount).await?;
    dispute(e, j).await?;
    Ok(j)
}

async fn withdraw(
    e: &Env,
    owner: &Account,
    agent: u64,
    amount: u128,
    g: u64,
    receiver: Option<&AccountId>,
) -> anyhow::Result<ExecutionFinalResult> {
    let mut args = json!({ "agent_id": agent, "amount": amount.to_string() });
    if let Some(r) = receiver {
        args["receiver"] = json!(r);
    }
    Ok(owner
        .call(e.pactora.id(), "withdraw")
        .args_json(args)
        .deposit(yocto(1))
        .gas(tgas(g))
        .transact()
        .await?)
}

// ═══════════════════════════ 1. callbacks and fake tokens ═══════════════════════════

#[tokio::test]
async fn callbacks_are_private_and_only_the_token_can_credit() -> anyhow::Result<()> {
    let (e, client, _) = market().await?;
    let mallory = sub(&e.root, "mallory", 20).await?;
    let before = totals(&e).await?;

    for refund in [
        json!({ "Purse": client }),
        json!("Fees"),
        json!({ "Stake": client }),
    ] {
        let r = call(
            &mallory,
            e.pactora.id(),
            "on_send",
            json!({ "receiver": mallory.id(), "amount": "1000000", "refund": refund }),
        )
        .await?;
        refused(&r, "Method on_send is private", "outsider on_send");
    }
    let r = call(
        &mallory,
        e.pactora.id(),
        "on_storage_sent",
        json!({ "agent_id": client, "amount": "1000000000000000000000000" }),
    )
    .await?;
    refused(
        &r,
        "Method on_storage_sent is private",
        "outsider on_storage_sent",
    );

    // ft_on_transfer called directly, pretending to be the token.
    let r = call(
        &mallory,
        e.pactora.id(),
        "ft_on_transfer",
        json!({ "sender_id": mallory.id(), "amount": "5000000", "msg": json!({"Fund":{"agent_id": client}}).to_string() }),
    )
    .await?;
    refused(&r, "WrongToken", "direct ft_on_transfer");

    // A look-alike token (same code, other account) sends through ft_transfer_call.
    let fake_wasm = wasm_from("MOCK_USDC_WASM", "../mock-usdc").await?;
    let fake = sub(&e.root, "fakeusdc", 10)
        .await?
        .deploy(&fake_wasm)
        .await?
        .into_result()?;
    ok(
        fake.call("new")
            .args_json(json!({ "owner": fake.id() }))
            .transact()
            .await?,
        "fake new",
    );
    for a in [&mallory, e.pactora.as_account()] {
        ok(
            a.call(fake.id(), "storage_deposit")
                .args_json(json!({ "account_id": a.id() }))
                .deposit(NearToken::from_millinear(10))
                .transact()
                .await?,
            "fake storage",
        );
    }
    ok(
        fake.call("mint")
            .args_json(json!({ "account_id": mallory.id(), "amount": "9000000" }))
            .transact()
            .await?,
        "fake mint",
    );
    let r = mallory
        .call(fake.id(), "ft_transfer_call")
        .args_json(json!({ "receiver_id": e.pactora.id(), "amount": "9000000", "msg": json!({"Fund":{"agent_id": client}}).to_string() }))
        .deposit(yocto(1))
        .max_gas()
        .transact()
        .await?;
    let fake_bal: String = fake
        .view("ft_balance_of")
        .args_json(json!({ "account_id": mallory.id() }))
        .await?
        .json()?;
    assert_eq!(fake_bal, "9000000", "the fake token refunds its sender");
    assert!(
        r.receipt_outcomes()
            .iter()
            .any(|o| &o.executor_id == e.pactora.id() && o.is_failure()),
        "pactora's ft_on_transfer refused the fake token"
    );
    assert_eq!(totals(&e).await?, before, "no bucket moved");
    check_i1(&e).await?;
    Ok(())
}

// ═══════════════════════════ 2. failed transfers ═══════════════════════════

#[tokio::test]
async fn every_failed_transfer_restores_its_bucket() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();

    // (a) withdraw to an account that does not exist.
    let ghost: AccountId = format!("ghost.{}", e.root.id()).parse()?;
    let before = balance_of(&e, client).await?;
    let r = ok(
        withdraw(&e, &e.alice, client, 1_000_000, 100, Some(&ghost)).await?,
        "withdraw ghost",
    );
    let evs = events(&r, &pid);
    assert!(!r.json::<bool>()?, "on_send reports failure");
    assert_eq!(balance_of(&e, client).await?, before);
    assert_eq!(names(&evs), ["transfer_started", "transfer_finished"]);
    assert_eq!(field(&evs, "transfer_finished")["ok"], false);
    check_i1(&e).await?;

    // (b) fees to a recipient that is not registered on the token.
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    ok(
        call(&e.cw, &pid, "approve", json!({ "job_id": j })).await?,
        "approve",
    );
    let unreg = sub(&e.root, "unreg", 5).await?;
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "set_fee_recipient",
            json!({ "fee_recipient": unreg.id() }),
        )
        .await?,
        "set_fee_recipient",
    );
    let owed = n(&totals(&e).await?["fees_owed"]);
    assert!(owed > 0);
    let r = ok(
        e.alice
            .call(&pid, "claim_fees")
            .max_gas()
            .transact()
            .await?,
        "claim_fees",
    );
    assert!(!r.json::<bool>()?);
    assert_eq!(
        n(&totals(&e).await?["fees_owed"]),
        owed,
        "fees back in fees_owed"
    );
    check_i1(&e).await?;

    // (c) matured stake to an owner the token does not know.
    let carol = sub(&e.root, "carol", 20).await?;
    let cwallet = sub(&carol, "w", 5).await?;
    let cagent = register(&e, &carol, &cwallet, 1).await?;
    send_in(
        &e,
        &e.bob,
        3_000_000,
        json!({ "Stake": { "agent_id": cagent } }),
    )
    .await?;
    ok(
        call1(
            &carol,
            &pid,
            "request_unstake",
            json!({ "agent_id": cagent, "amount": "3000000" }),
        )
        .await?,
        "request_unstake",
    );
    let unlock = n(&stake_of(&e, cagent).await?["unlock_at"]) as u64;
    wait_past(&e, unlock).await?;
    let r = ok(
        carol
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": cagent }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "complete_unstake",
    );
    let evs = events(&r, &pid);
    assert!(!r.json::<bool>()?);
    let s = stake_of(&e, cagent).await?;
    assert_eq!(s["available"], "3000000", "stake back in available: {s}");
    assert_eq!(s["pending"], "0");
    println!(
        "complete_unstake to an unregistered owner emitted (successful receipts): {:?}",
        names(&evs)
    );
    // The stake never left, so nothing may say it did.
    assert!(
        !names(&evs).iter().any(|x| x == "unstaked"),
        "unstaked emitted for a payout that failed: {evs:?}"
    );
    assert_eq!(
        names(&evs)
            .iter()
            .filter(|x| *x == "stake_restored")
            .count(),
        1,
        "{evs:?}"
    );
    assert_eq!(field(&evs, "stake_restored")["amount"], "3000000");
    check_i1(&e).await?;

    // And when it lands, `unstaked` is in the callback's receipt, after the transfer.
    ok(
        call1(
            &e.bob,
            &pid,
            "request_unstake",
            json!({ "agent_id": worker, "amount": "1000000" }),
        )
        .await?,
        "request_unstake bob",
    );
    let unlock = n(&stake_of(&e, worker).await?["unlock_at"]) as u64;
    wait_past(&e, unlock).await?;
    let r = ok(
        e.bob
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": worker }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "complete_unstake bob",
    );
    let evs = events(&r, &pid);
    assert!(r.json::<bool>()?);
    assert_eq!(
        names(&evs),
        ["transfer_started", "unstaked", "transfer_finished"],
        "{evs:?}"
    );
    assert_eq!(field(&evs, "unstaked")["amount"], "1000000");
    check_i1(&e).await?;
    Ok(())
}

// ═══════════════════════════ 3. deleted owner account ═══════════════════════════

#[tokio::test]
async fn storage_withdraw_to_an_owner_deleted_in_the_same_block() -> anyhow::Result<()> {
    let e = setup().await?;
    let pid = e.pactora.id().clone();
    let carol = sub(&e.root, "carol", 20).await?;
    let wallet = sub(&e.root, "carolw", 5).await?;
    let agent = register(&e, &carol, &wallet, 5).await?;
    let prepaid = storage_bal(&e, agent).await?;
    let own_before = contract_own_near(&e).await?;

    let s1 = carol
        .call(&pid, "storage_withdraw")
        .args_json(json!({ "agent_id": agent, "amount": prepaid.to_string() }))
        .deposit(yocto(1))
        .gas(tgas(100))
        .transact_async()
        .await?;
    let s2 = carol
        .batch(carol.id())
        .delete_account(e.root.id())
        .transact_async()
        .await?;
    let r1 = s1.await?;
    let r2 = s2.await?;
    assert!(r2.is_success(), "delete_account: {r2:?}");
    let exists = e.worker.view_account(carol.id()).await.is_ok();
    let r1_ok = r1.is_success();
    let sent: Option<bool> = r1.json().ok();
    let after = storage_bal(&e, agent).await?;
    println!("storage_withdraw raced with delete_account: call ok={} callback={sent:?} storage_balance {prepaid} -> {after}, carol exists={exists}", r1_ok);
    if r1_ok && sent == Some(false) {
        assert_eq!(after, prepaid, "refund restored the storage balance");
    }
    // Whatever path was taken, the contract never paid for it.
    assert!(
        contract_own_near(&e).await? >= own_before,
        "contract lost NEAR"
    );

    // The name comes back under its parent: whoever recreates it owns the agent.
    let carol2 = sub(&e.root, "carol", 5).await?;
    let left = storage_bal(&e, agent).await?;
    if left > 0 {
        let b0 = near_of(&e, carol2.id()).await?;
        let r = ok(
            call1(
                &carol2,
                &pid,
                "storage_withdraw",
                json!({ "agent_id": agent, "amount": left.to_string() }),
            )
            .await?,
            "recreated owner withdraws",
        );
        assert!(r.json::<bool>()?);
        println!("recreated carol took {left} yocto of the deleted owner's storage balance (NEAR delta {})", near_of(&e, carol2.id()).await? as i128 - b0 as i128);
    }
    Ok(())
}

// ═══════════════════════════ 4. races in one block ═══════════════════════════

#[tokio::test]
async fn racing_transactions_never_pay_twice() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();

    // Double withdraw of the whole balance, same block.
    let bal = balance_of(&e, client).await?;
    let tok0 = ft_balance(&e, e.alice.id()).await?;
    let mk = || {
        e.alice
            .call(&pid, "withdraw")
            .args_json(json!({ "agent_id": client, "amount": (bal - 5_000_000).to_string() }))
            .deposit(yocto(1))
            .max_gas()
    };
    let (a, b) = (mk().transact_async().await?, mk().transact_async().await?);
    let (ra, rb) = (a.await?, b.await?);
    assert_eq!(
        [ra.is_success(), rb.is_success()]
            .iter()
            .filter(|x| **x)
            .count(),
        1,
        "exactly one withdraw: {ra:?} {rb:?}"
    );
    let loser = if ra.is_success() { &rb } else { &ra };
    refused(loser, "InsufficientBalance", "second withdraw");
    assert_eq!(ft_balance(&e, e.alice.id()).await? - tok0, bal - 5_000_000);
    check_i1(&e).await?;

    // Approve and dispute racing from the client's wallet.
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    let a =
        e.cw.call(&pid, "approve")
            .args_json(json!({ "job_id": j }))
            .gas(tgas(100))
            .transact_async()
            .await?;
    let b =
        e.cw.call(&pid, "dispute")
            .args_json(json!({ "job_id": j }))
            .gas(tgas(100))
            .transact_async()
            .await?;
    let (ra, rb) = (a.await?, b.await?);
    assert_eq!(
        [ra.is_success(), rb.is_success()]
            .iter()
            .filter(|x| **x)
            .count(),
        1
    );
    check_i1(&e).await?;

    // Two claim_fees in one block.
    let fees0 = ft_balance(&e, e.fees.id()).await?;
    let owed = n(&totals(&e).await?["fees_owed"]);
    let a = e
        .alice
        .call(&pid, "claim_fees")
        .max_gas()
        .transact_async()
        .await?;
    let b = e
        .bob
        .call(&pid, "claim_fees")
        .max_gas()
        .transact_async()
        .await?;
    let (ra, rb) = (a.await?, b.await?);
    assert_eq!(
        [ra.is_success(), rb.is_success()]
            .iter()
            .filter(|x| **x)
            .count(),
        1
    );
    assert_eq!(ft_balance(&e, e.fees.id()).await? - fees0, owed);
    check_i1(&e).await?;

    // A late ruling for the client racing the worker's expire_dispute, just
    // after the deadline: inside the grace the expiry is refused whatever
    // the order, so the ruling wins. (Before the fix the worker could expire
    // the dispute in the same block and be paid.)
    let g = grace(&e).await?;
    let j = disputed_job(&e, client, worker, 1_000_000).await?;
    let dl = n(&job(&e, j).await?["dispute_deadline"]) as u64;
    wait_past(&e, dl).await?;
    assert!(
        now_s(&e).await? <= dl + g,
        "the grace ({g} s) must still be running"
    );
    let a = e
        .dao
        .call(&pid, "resolve_dispute")
        .args_json(json!({ "job_id": j, "for_worker": false }))
        .gas(tgas(100))
        .transact_async()
        .await?;
    let b =
        e.ww.call(&pid, "expire_dispute")
            .args_json(json!({ "job_id": j }))
            .gas(tgas(100))
            .transact_async()
            .await?;
    let (ra, rb) = (a.await?, b.await?);
    println!(
        "ruling vs expire_dispute race inside the grace: ruling ok={} expire ok={} -> state {}",
        ra.is_success(),
        rb.is_success(),
        job(&e, j).await?["state"]
    );
    ok(ra, "the late ruling");
    // The two land in either order. Expiry first: refused inside the grace
    // (`DeadlineNotPassed`). Ruling first: the job is already Refunded, so the
    // expiry is refused as `InvalidState` (seen 2026-11-10). Either way it is
    // refused and the ruling ends the job, which the next assert checks.
    assert!(
        rb.is_failure(),
        "expire_dispute inside the grace must fail but succeeded"
    );
    let rbs = format!("{rb:?}");
    assert!(
        rbs.contains("DeadlineNotPassed") || rbs.contains("InvalidState"),
        "expire_dispute inside the grace: expected `DeadlineNotPassed` or `InvalidState`, got {rbs}"
    );
    assert_eq!(
        job(&e, j).await?["state"],
        "Refunded",
        "the ruling, not the expiry, ended the job"
    );
    check_i1(&e).await?;
    // After the grace anyone may expire it, and the bond is split.
    let j = disputed_job(&e, client, worker, 1_000_000).await?;
    let dl = n(&job(&e, j).await?["dispute_deadline"]) as u64;
    let (c0, w0) = (balance_of(&e, client).await?, balance_of(&e, worker).await?);
    wait_past(&e, dl + g).await?;
    let r = ok(
        call(&e.ww, &pid, "expire_dispute", json!({ "job_id": j })).await?,
        "expire_dispute after the grace",
    );
    let d = field(&events(&r, &pid), "dispute_expired").clone();
    assert_eq!(
        (d["bond_to_worker"].as_str(), d["bond_to_client"].as_str()),
        (Some("50000"), Some("50000")),
        "{d}"
    );
    assert_eq!(
        balance_of(&e, worker).await? - w0,
        990_000 + 50_000,
        "the job and half the bond"
    );
    assert_eq!(
        balance_of(&e, client).await? - c0,
        50_000,
        "half the bond back"
    );
    check_i1(&e).await?;

    // Two complete_unstake for one matured request.
    ok(
        call1(
            &e.bob,
            &pid,
            "request_unstake",
            json!({ "agent_id": worker, "amount": "1000000" }),
        )
        .await?,
        "request_unstake",
    );
    let unlock = n(&stake_of(&e, worker).await?["unlock_at"]) as u64;
    wait_past(&e, unlock).await?;
    let bob0 = ft_balance(&e, e.bob.id()).await?;
    let mk = || {
        e.bob
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": worker }))
            .deposit(yocto(1))
            .max_gas()
    };
    let (a, b) = (mk().transact_async().await?, mk().transact_async().await?);
    let (ra, rb) = (a.await?, b.await?);
    assert_eq!(
        [ra.is_success(), rb.is_success()]
            .iter()
            .filter(|x| **x)
            .count(),
        1
    );
    assert_eq!(ft_balance(&e, e.bob.id()).await? - bob0, 1_000_000);
    check_i1(&e).await?;

    // Two storage_withdraw of the whole prepaid balance.
    let sb = storage_bal(&e, client).await?;
    let mk = || {
        e.alice
            .call(&pid, "storage_withdraw")
            .args_json(json!({ "agent_id": client, "amount": sb.to_string() }))
            .deposit(yocto(1))
            .gas(tgas(100))
    };
    let (a, b) = (mk().transact_async().await?, mk().transact_async().await?);
    let (ra, rb) = (a.await?, b.await?);
    assert_eq!(
        [ra.is_success(), rb.is_success()]
            .iter()
            .filter(|x| **x)
            .count(),
        1
    );
    assert_eq!(storage_bal(&e, client).await?, 0);
    Ok(())
}

// ═══════════════════════════ 5. prepaid gas sweeps ═══════════════════════════

#[tokio::test]
async fn gas_sweep_never_strands_money_in_flight() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let mut first_ok = None;
    let mut max_cb = 0u64;
    for g in [
        3u64, 5, 8, 10, 15, 20, 25, 28, 30, 31, 32, 33, 34, 35, 36, 38, 40, 45, 50, 60, 80, 150,
        300,
    ] {
        let b0 = balance_of(&e, client).await?;
        let t0 = ft_balance(&e, e.alice.id()).await?;
        let r = withdraw(&e, &e.alice, client, 1, g, None).await?;
        let b1 = balance_of(&e, client).await?;
        let t1 = ft_balance(&e, e.alice.id()).await?;
        for o in r.receipt_outcomes() {
            if o.executor_id == pid && o.logs.iter().any(|l| l.contains("transfer_finished")) {
                max_cb = max_cb.max(o.gas_burnt.as_gas());
            }
        }
        let cb_failed = r
            .receipt_outcomes()
            .iter()
            .skip(1)
            .any(|o| o.executor_id == pid && o.is_failure());
        println!("withdraw gas {g:>3} Tgas: success={} purse {b0}->{b1} token +{} callback_failed={cb_failed}", r.is_success(), t1 - t0);
        assert!(!cb_failed, "on_send failed at {g} Tgas: {r:?}");
        assert_eq!(
            b0 - b1,
            t1 - t0,
            "purse and token moved together at {g} Tgas"
        );
        if r.is_success() && first_ok.is_none() {
            first_ok = Some(g);
        }
        check_i1(&e).await?;
    }
    println!(
        "withdraw first succeeds at {first_ok:?} Tgas; on_send burns at most {:.2} Tgas",
        max_cb as f64 / 1e12
    );

    // claim_fees and complete_unstake near their threshold.
    for _ in 0..3 {
        let j = submitted_job(&e, client, worker, 100_000).await?;
        ok(
            call(&e.cw, &pid, "approve", json!({ "job_id": j })).await?,
            "approve",
        );
    }
    for g in [20u64, 30, 31, 32, 34, 36, 40] {
        let owed = n(&totals(&e).await?["fees_owed"]);
        let r = e
            .alice
            .call(&pid, "claim_fees")
            .gas(tgas(g))
            .transact()
            .await?;
        println!(
            "claim_fees gas {g}: success={} fees_owed {owed} -> {}",
            r.is_success(),
            totals(&e).await?["fees_owed"]
        );
        check_i1(&e).await?;
        if r.is_success() {
            break;
        }
        assert_eq!(n(&totals(&e).await?["fees_owed"]), owed);
    }
    ok(
        call1(
            &e.bob,
            &pid,
            "request_unstake",
            json!({ "agent_id": worker, "amount": "100" }),
        )
        .await?,
        "request_unstake",
    );
    let unlock = n(&stake_of(&e, worker).await?["unlock_at"]) as u64;
    wait_past(&e, unlock).await?;
    for g in [20u64, 30, 32, 34, 36, 40, 60] {
        let r = e
            .bob
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": worker }))
            .deposit(yocto(1))
            .gas(tgas(g))
            .transact()
            .await?;
        let s = stake_of(&e, worker).await?;
        println!(
            "complete_unstake gas {g}: success={} pending={}",
            r.is_success(),
            s["pending"]
        );
        check_i1(&e).await?;
        if r.is_success() {
            break;
        }
        assert_eq!(
            s["pending"], "100",
            "a failed complete_unstake keeps the request"
        );
    }

    // storage_withdraw has no explicit gas check: sweep it.
    for g in [3u64, 5, 8, 10, 12, 14, 16, 20, 30] {
        let sb0 = storage_bal(&e, client).await?;
        let own0 = contract_own_near(&e).await?;
        let r = e
            .alice
            .call(&pid, "storage_withdraw")
            .args_json(json!({ "agent_id": client, "amount": "1000000000000000000000" }))
            .deposit(yocto(1))
            .gas(tgas(g))
            .transact()
            .await?;
        let sb1 = storage_bal(&e, client).await?;
        let cb_failed = r
            .receipt_outcomes()
            .iter()
            .skip(1)
            .any(|o| o.executor_id == pid && o.is_failure());
        println!("storage_withdraw gas {g:>2}: success={} storage {sb0}->{sb1} callback_failed={cb_failed}", r.is_success());
        assert!(!cb_failed);
        assert!(contract_own_near(&e).await? >= own0);
    }

    // Funding through ft_transfer_call with little gas: the token must refund or we must credit.
    for g in [31u64, 33, 35, 38, 40, 45, 50, 60] {
        let a0 = ft_balance(&e, e.alice.id()).await?;
        let p0 = balance_of(&e, client).await?;
        let r = e.alice.call(e.usdc.id(), "ft_transfer_call")
            .args_json(json!({ "receiver_id": &pid, "amount": "1000", "msg": json!({"Fund":{"agent_id": client}}).to_string() }))
            .deposit(yocto(1)).gas(tgas(g)).transact().await?;
        let a1 = ft_balance(&e, e.alice.id()).await?;
        let p1 = balance_of(&e, client).await?;
        let held = ft_balance(&e, &pid).await?;
        let sum = n(&totals(&e).await?["sum"]);
        println!(
            "ft_transfer_call gas {g}: tx_ok={} alice -{} purse +{} surplus_in_contract={}",
            r.is_success(),
            a0 - a1,
            p1 - p0,
            held as i128 - sum as i128
        );
        assert_eq!(
            a0 - a1,
            p1 - p0,
            "debited exactly what was credited at {g} Tgas"
        );
        check_i1(&e).await?;
    }
    Ok(())
}

// ═══════════════════════════ 6. inputs and gas ceilings ═══════════════════════════

#[tokio::test]
async fn no_input_can_push_a_method_near_the_gas_limit() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let big = "x".repeat(512);
    let r = e
        .alice
        .call(&pid, "register_agent")
        .args_json(
            json!({ "wallet": format!("{}{}", "a".repeat(40), ".test.near"), "card_uri": big }),
        )
        .deposit(NearToken::from_near(1))
        .gas(tgas(300))
        .transact()
        .await?;
    let r = ok(r, "register 512-byte card");
    println!(
        "register_agent 512 B card: {:.2} Tgas",
        r.total_gas_burnt.as_gas() as f64 / 1e12
    );
    let r = e
        .alice
        .call(&pid, "register_agent")
        .args_json(json!({ "wallet": e.alice.id(), "card_uri": "x".repeat(513) }))
        .deposit(NearToken::from_near(1))
        .transact()
        .await?;
    refused(&r, "CardUriTooLong", "513-byte card");
    for bad in [
        "A".repeat(64),
        "1".repeat(63),
        "g".repeat(64),
        "1".repeat(100_000),
    ] {
        let r = e.cw.call(&pid, "create_job").args_json(json!({ "client_id": client, "worker_id": worker, "amount": "100000", "spec_hash": bad, "accept_window": 300, "work_window": 1800 })).gas(tgas(300)).transact().await;
        match r {
            Ok(r) => refused(&r, "BadHash", "bad spec hash"),
            Err(err) => println!("oversized args rejected before execution: {err}"),
        }
    }
    // Measure every exit.
    let mut gas = vec![];
    let j = create_job(&e, client, worker, 1_000_000, 300, 1800).await?;
    gas.push(("accept_job", accept(&e, j).await?.total_gas_burnt));
    gas.push(("submit_result", submit(&e, j).await?.total_gas_burnt));
    gas.push(("dispute", dispute(&e, j).await?.total_gas_burnt));
    gas.push((
        "resolve_dispute",
        ok(
            call(
                &e.dao,
                &pid,
                "resolve_dispute",
                json!({ "job_id": j, "for_worker": true }),
            )
            .await?,
            "resolve",
        )
        .total_gas_burnt,
    ));
    let j = create_job(&e, client, worker, 1_000_000, 1, 1).await?;
    wait_past(&e, n(&job(&e, j).await?["accept_deadline"]) as u64).await?;
    gas.push((
        "expire_unaccepted",
        ok(
            call(&e.alice, &pid, "expire_unaccepted", json!({ "job_id": j })).await?,
            "expire_unaccepted",
        )
        .total_gas_burnt,
    ));
    for (m, g) in &gas {
        println!("{m:<18} {:.2} Tgas", g.as_gas() as f64 / 1e12);
        assert!(g.as_gas() < 30_000_000_000_000, "{m} uses a lot of gas");
    }
    Ok(())
}

// ═══════════════════════════ 7. deposits ═══════════════════════════

#[tokio::test]
async fn exact_yocto_rules_and_overpayment_is_refundable() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let owner = e.pactora.as_account();
    let one_yocto: Vec<(&Account, &str, Value)> = vec![
        (owner, "propose_owner", json!({ "new_owner": e.alice.id() })),
        (owner, "set_arbiter", json!({ "arbiter": e.dao.id() })),
        (owner, "set_pauser", json!({ "pauser": e.dao.id() })),
        (
            owner,
            "set_fee_recipient",
            json!({ "fee_recipient": e.fees.id() }),
        ),
        (owner, "set_params", json!({ "params": params() })),
        (owner, "unpause", json!({})),
        (owner, "recover_surplus", json!({ "amount": "1" })),
        (
            &e.alice,
            "propose_wallet",
            json!({ "agent_id": client, "wallet": null }),
        ),
        (
            &e.alice,
            "set_card_uri",
            json!({ "agent_id": client, "card_uri": "x" }),
        ),
        (&e.alice, "unfreeze_agent", json!({ "agent_id": client })),
        (
            &e.alice,
            "set_caps",
            json!({ "agent_id": client, "per_task_cap": "1", "daily_cap": "1" }),
        ),
        (
            &e.alice,
            "storage_withdraw",
            json!({ "agent_id": client, "amount": "1" }),
        ),
        (
            &e.alice,
            "withdraw",
            json!({ "agent_id": client, "amount": "1" }),
        ),
        (
            &e.bob,
            "request_unstake",
            json!({ "agent_id": worker, "amount": "1" }),
        ),
        (&e.bob, "cancel_unstake", json!({ "agent_id": worker })),
        (&e.bob, "complete_unstake", json!({ "agent_id": worker })),
    ];
    for (who, m, args) in &one_yocto {
        for dep in [0u128, 2] {
            let r = who
                .call(&pid, m)
                .args_json(args.clone())
                .deposit(yocto(dep))
                .gas(tgas(100))
                .transact()
                .await?;
            refused(
                &r,
                "Requires attached deposit of exactly 1 yoctoNEAR",
                &format!("{m} with {dep} yocto"),
            );
        }
    }
    let non_payable: Vec<(&Account, &str, Value)> = vec![
        (
            &e.cw,
            "create_job",
            json!({ "client_id": client, "worker_id": worker, "amount": "100000", "spec_hash": SPEC, "accept_window": 300, "work_window": 1800 }),
        ),
        (&e.ww, "accept_job", json!({ "job_id": 1 })),
        (&e.cw, "approve", json!({ "job_id": 1 })),
        (&e.alice, "claim_fees", json!({})),
        (owner, "pause", json!({})),
        (&e.alice, "freeze_agent", json!({ "agent_id": client })),
        (&e.cw, "accept_wallet", json!({ "agent_id": client })),
        (
            &e.cw,
            "direct_pay",
            json!({ "client_id": client, "worker_id": worker, "amount": "20000", "spec_hash": SPEC }),
        ),
    ];
    for (who, m, args) in &non_payable {
        let r = who
            .call(&pid, m)
            .args_json(args.clone())
            .deposit(yocto(1))
            .gas(tgas(100))
            .transact()
            .await?;
        refused(&r, "doesn't accept deposit", &format!("{m} with 1 yocto"));
    }

    // Overpaying register_agent: the excess is the agent's, and comes back.
    let carol = sub(&e.root, "carol", 30).await?;
    let w = sub(&carol, "w", 2).await?;
    let id: u64 = carol
        .call(&pid, "register_agent")
        .args_json(json!({ "wallet": w.id(), "card_uri": "c" }))
        .deposit(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?
        .json()?;
    let sb = storage_bal(&e, id).await?;
    let cost = NearToken::from_near(10).as_yoctonear() - sb;
    println!("register_agent storage cost {cost} yocto; excess kept as storage balance {sb}");
    let own0 = contract_own_near(&e).await?;
    let c0 = near_of(&e, carol.id()).await?;
    let p0 = near_of(&e, &pid).await?;
    let r = ok(
        call1(
            &carol,
            &pid,
            "storage_withdraw",
            json!({ "agent_id": id, "amount": sb.to_string() }),
        )
        .await?,
        "storage_withdraw",
    );
    let sent: u128 = r
        .receipt_outcomes()
        .iter()
        .filter(|o| &o.executor_id == carol.id())
        .count() as u128;
    assert!(r.json::<bool>()?);
    let got = near_of(&e, carol.id()).await? as i128 - c0 as i128;
    let paid = p0 as i128 - near_of(&e, &pid).await? as i128;
    // The contract pays out exactly the excess (less the gas reward it earns);
    // carol's own delta also carries her prepaid gas, which the runtime keeps.
    println!("storage_withdraw of {sb}: contract paid {paid}, carol's NEAR delta {got} ({sent} receipts on carol)");
    assert!(
        paid <= sb as i128 && paid > sb as i128 - 10_000_000_000_000_000_000_000,
        "contract paid the excess once"
    );
    assert!(contract_own_near(&e).await? >= own0);

    // Underpaying by one yocto is refused; paying exactly leaves zero and the wallet can't accept yet.
    let w2 = sub(&carol, "w2", 2).await?;
    let cost = cost + BYTE_COST; // "w2" is one byte longer than "w"
    let r = carol
        .call(&pid, "register_agent")
        .args_json(json!({ "wallet": w2.id(), "card_uri": "c" }))
        .deposit(yocto(cost - 1))
        .transact()
        .await?;
    refused(&r, "InsufficientStorage", "register one yocto short");
    let id2: u64 = carol
        .call(&pid, "register_agent")
        .args_json(json!({ "wallet": w2.id(), "card_uri": "c" }))
        .deposit(yocto(cost))
        .transact()
        .await?
        .into_result()?
        .json()?;
    assert_eq!(storage_bal(&e, id2).await?, 0);
    let r = call(&w2, &pid, "accept_wallet", json!({ "agent_id": id2 })).await?;
    refused(
        &r,
        "InsufficientStorage",
        "accept_wallet with no storage balance",
    );
    // Anyone may top it up, even with zero.
    ok(
        e.bob
            .call(&pid, "storage_deposit")
            .args_json(json!({ "agent_id": id2 }))
            .transact()
            .await?,
        "storage_deposit 0",
    );
    // Plain ft_transfer into the contract: tokens nobody can ever recover.
    ok(
        e.alice
            .call(e.usdc.id(), "ft_transfer")
            .args_json(json!({ "receiver_id": &pid, "amount": "777" }))
            .deposit(yocto(1))
            .transact()
            .await?,
        "plain ft_transfer",
    );
    let held = ft_balance(&e, &pid).await?;
    let sum = n(&totals(&e).await?["sum"]);
    println!("after a plain ft_transfer of 777: token balance {held}, totals {sum}, unrecoverable surplus {}", held - sum);
    assert_eq!(held - sum, 777);
    Ok(())
}

// ═══════════════════════════ 8. storage spam ═══════════════════════════

#[tokio::test]
async fn spam_never_makes_the_contract_pay_storage() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let mut own = contract_own_near(&e).await?;
    // Reserved growth bytes are billed at create time but used later, so the
    // contract's own NEAR may dip after a job grows — never below where it
    // started.
    let base = own;
    let check = |label: String, _before: i128, after: i128| {
        assert!(
            after >= base,
            "{label}: contract's own NEAR fell below its start {base} -> {after}"
        );
        after
    };

    // A spammer with a client of its own, minimal storage.
    let sp = sub(&e.root, "spammer", 50).await?;
    let spw = sub(&sp, "w", 5).await?;
    let sid: u64 = sp
        .call(&pid, "register_agent")
        .args_json(json!({ "wallet": spw.id(), "card_uri": "" }))
        .deposit(NearToken::from_millinear(30))
        .transact()
        .await?
        .into_result()?
        .json()?;
    own = check("register".into(), own, contract_own_near(&e).await?);
    ok(
        call(&spw, &pid, "accept_wallet", json!({ "agent_id": sid })).await?,
        "spam accept_wallet",
    );
    own = check("accept_wallet".into(), own, contract_own_near(&e).await?);
    send_in(
        &e,
        &e.alice,
        5_000_000,
        json!({ "Fund": { "agent_id": sid } }),
    )
    .await?;
    own = check("fund".into(), own, contract_own_near(&e).await?);

    let mut made = 0;
    loop {
        let r = call(&spw, &pid, "create_job", json!({ "client_id": sid, "worker_id": worker, "amount": "10000", "spec_hash": SPEC, "accept_window": 300, "work_window": 1800 })).await?;
        if r.is_failure() {
            refused(&r, "InsufficientStorage", "spam create_job");
            assert!(
                events(&r, &pid).is_empty(),
                "no event from a refused create_job"
            );
            println!(
                "create_job refused after {made} jobs; failed-receipt logs: {}",
                failed_logs(&r).len()
            );
            break;
        }
        made += 1;
        own = check(format!("job {made}"), own, contract_own_near(&e).await?);
        assert!(made < 200);
    }
    // Cancel them all, and drive growth on the honest pair.
    for j in (n(&view(&e, "job_count", json!({})).await?) as u64 - made + 1)
        ..=(n(&view(&e, "job_count", json!({})).await?) as u64)
    {
        ok(
            call(&spw, &pid, "cancel", json!({ "job_id": j })).await?,
            "cancel",
        );
        own = check(format!("cancel {j}"), own, contract_own_near(&e).await?);
    }
    // A job's whole life fits inside what its client was billed at creation.
    for path in ["approve+hash", "dispute+reason"] {
        let u0 = e.worker.view_account(&pid).await?.storage_usage;
        let s0 = storage_bal(&e, client).await?;
        let j = submitted_job(&e, client, worker, 1_000_000).await?;
        let billed = s0 - storage_bal(&e, client).await?;
        if path == "approve+hash" {
            ok(
                call(
                    &e.cw,
                    &pid,
                    "approve",
                    json!({ "job_id": j, "review_hash": REASON }),
                )
                .await?,
                "approve with review hash",
            );
        } else {
            dispute(&e, j).await?;
            ok(
                call(
                    &e.dao,
                    &pid,
                    "resolve_dispute",
                    json!({ "job_id": j, "for_worker": false }),
                )
                .await?,
                "resolve",
            );
        }
        let grown = (e.worker.view_account(&pid).await?.storage_usage - u0) as u128 * BYTE_COST;
        println!(
            "{path}: job used {} bytes over its life, client billed {} bytes",
            grown / BYTE_COST,
            billed / BYTE_COST
        );
        assert!(
            grown <= billed,
            "{path}: storage grew beyond what was billed"
        );
        own = check(path.into(), own, contract_own_near(&e).await?);
    }
    ok(call(&e.cw, &pid, "direct_pay", json!({ "client_id": client, "worker_id": worker, "amount": "20000", "spec_hash": SPEC })).await?, "direct_pay");
    own = check("direct_pay".into(), own, contract_own_near(&e).await?);
    // Wallet rotation to a 64-char account and back; card growth.
    let long: AccountId = format!(
        "{}.{}",
        "z".repeat(64 - 1 - e.root.id().as_str().len()),
        e.root.id()
    )
    .parse()?;
    let lw = e
        .root
        .create_subaccount(long.as_str().split('.').next().unwrap())
        .initial_balance(NearToken::from_near(2))
        .transact()
        .await?
        .into_result()?;
    ok(
        call1(
            &e.alice,
            &pid,
            "propose_wallet",
            json!({ "agent_id": client, "wallet": lw.id() }),
        )
        .await?,
        "propose long wallet",
    );
    own = check("propose_wallet".into(), own, contract_own_near(&e).await?);
    ok(
        call(&lw, &pid, "accept_wallet", json!({ "agent_id": client })).await?,
        "accept long wallet",
    );
    own = check("rotate wallet".into(), own, contract_own_near(&e).await?);
    ok(
        call1(
            &e.alice,
            &pid,
            "set_card_uri",
            json!({ "agent_id": client, "card_uri": "y".repeat(512) }),
        )
        .await?,
        "set_card_uri 512",
    );
    check("card 512".into(), own, contract_own_near(&e).await?);
    check_i1(&e).await?;
    Ok(())
}

// ═══════════════════════════ 9. roles ═══════════════════════════

#[tokio::test]
async fn strangers_and_parties_hold_no_roles() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let m = sub(&e.root, "mallory", 20).await?;
    for (meth, args) in [
        ("propose_owner", json!({ "new_owner": m.id() })),
        ("set_arbiter", json!({ "arbiter": m.id() })),
        ("set_pauser", json!({ "pauser": m.id() })),
        ("set_fee_recipient", json!({ "fee_recipient": m.id() })),
        ("set_params", json!({ "params": params() })),
        ("unpause", json!({})),
        ("recover_surplus", json!({ "amount": "1" })),
    ] {
        refused(&call1(&m, &pid, meth, args).await?, "NotOwner", meth);
    }
    refused(
        &call(&m, &pid, "pause", json!({})).await?,
        "NotPauser",
        "pause",
    );
    refused(
        &call1(&m, &pid, "accept_owner", json!({})).await?,
        "NotPendingOwner",
        "accept_owner",
    );
    // `new` and `migrate` are private: only the contract account, which is also
    // the only one that can deploy code, may call them.
    let r = call(&m, &pid, "new", json!({ "owner": m.id(), "token": m.id(), "arbiter": m.id(), "pauser": m.id(), "fee_recipient": m.id(), "params": params() })).await?;
    refused(&r, "Method new is private", "re-init by a stranger");
    let r = e.pactora.call("new").args_json(json!({ "owner": m.id(), "token": m.id(), "arbiter": m.id(), "pauser": m.id(), "fee_recipient": m.id(), "params": params() })).transact().await?;
    refused(
        &r,
        "already been initialized",
        "re-init by the contract account",
    );
    refused(
        &call(&m, &pid, "migrate", json!({})).await?,
        "Method migrate is private",
        "migrate by a stranger",
    );
    refused(
        &e.pactora.call("migrate").transact().await?,
        "StateIsNotV0",
        "migrate on an already-current state",
    );
    for meth in ["upgrade", "update_contract", "deploy"] {
        refused(
            &call1(&m, &pid, meth, json!({})).await?,
            "MethodNotFound",
            meth,
        );
    }
    for (meth, args, dep) in [
        (
            "propose_wallet",
            json!({ "agent_id": client, "wallet": m.id() }),
            1,
        ),
        (
            "set_card_uri",
            json!({ "agent_id": client, "card_uri": "pwned" }),
            1,
        ),
        ("freeze_agent", json!({ "agent_id": client }), 0),
        ("unfreeze_agent", json!({ "agent_id": client }), 1),
        (
            "set_caps",
            json!({ "agent_id": client, "per_task_cap": "999999999", "daily_cap": "999999999" }),
            1,
        ),
        (
            "storage_withdraw",
            json!({ "agent_id": client, "amount": "1" }),
            1,
        ),
        (
            "withdraw",
            json!({ "agent_id": client, "amount": "1", "receiver": m.id() }),
            1,
        ),
        (
            "request_unstake",
            json!({ "agent_id": worker, "amount": "1" }),
            1,
        ),
        ("cancel_unstake", json!({ "agent_id": worker }), 1),
        ("complete_unstake", json!({ "agent_id": worker }), 1),
    ] {
        let r = m
            .call(&pid, meth)
            .args_json(args)
            .deposit(yocto(dep))
            .max_gas()
            .transact()
            .await?;
        refused(&r, "NotAgentOwner", meth);
        // The agent's own wallet is not its owner either.
    }
    let r =
        e.cw.call(&pid, "freeze_agent")
            .args_json(json!({ "agent_id": client }))
            .transact()
            .await?;
    refused(&r, "NotAgentOwner", "wallet freezes its own agent");
    // Owners are not wallets.
    let j = create_job(&e, client, worker, 100_000, 300, 1800).await?;
    refused(
        &call(&e.bob, &pid, "accept_job", json!({ "job_id": j })).await?,
        "NotAgentWallet",
        "owner accepts",
    );
    refused(
        &call(&e.cw, &pid, "accept_job", json!({ "job_id": j })).await?,
        "NotAgentWallet",
        "client accepts its own job",
    );
    // Ruling: only the arbiter, never a party, and only on a disputed job.
    refused(
        &call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": true }),
        )
        .await?,
        "InvalidState",
        "rule on a Created job",
    );
    accept(&e, j).await?;
    submit(&e, j).await?;
    refused(
        &call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": true }),
        )
        .await?,
        "InvalidState",
        "rule on a Submitted job",
    );
    dispute(&e, j).await?;
    for who in [&e.alice, &e.bob, &e.cw, &e.ww, e.pactora.as_account(), &m] {
        refused(
            &call(
                who,
                &pid,
                "resolve_dispute",
                json!({ "job_id": j, "for_worker": true }),
            )
            .await?,
            "NotArbiter",
            "party rules",
        );
    }
    ok(
        call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": true }),
        )
        .await?,
        "arbiter rules",
    );
    refused(
        &call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": false }),
        )
        .await?,
        "InvalidState",
        "rule twice",
    );
    // Two-step ownership: the old owner keeps nothing.
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "propose_owner",
            json!({ "new_owner": e.alice.id() }),
        )
        .await?,
        "propose",
    );
    refused(
        &call1(&m, &pid, "accept_owner", json!({})).await?,
        "NotPendingOwner",
        "stranger accepts",
    );
    ok(
        call1(&e.alice, &pid, "accept_owner", json!({})).await?,
        "accept_owner",
    );
    refused(
        &call1(
            e.pactora.as_account(),
            &pid,
            "set_arbiter",
            json!({ "arbiter": m.id() }),
        )
        .await?,
        "NotOwner",
        "old owner",
    );
    // The contract account's key is still the real upgrade authority.
    let keys = e.pactora.view_access_keys().await?;
    println!("pactora account still has {} access key(s) after ownership moved to alice; whoever holds one can redeploy the code", keys.len());
    check_i1(&e).await?;
    Ok(())
}

// ═══════════════════════════ 10. real Sputnik DAO ═══════════════════════════

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let v = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn ruling_kind(pactora: &AccountId, job: u64, for_worker: bool) -> Value {
    json!({ "FunctionCall": { "receiver_id": pactora, "actions": [{
        "method_name": "resolve_dispute",
        "args": b64(json!({ "job_id": job, "for_worker": for_worker }).to_string().as_bytes()),
        "deposit": "0", "gas": "50000000000000",
    }]}})
}

async fn propose(m: &Account, dao: &AccountId, kind: &Value) -> anyhow::Result<u64> {
    let r = m
        .call(dao, "add_proposal")
        .args_json(json!({ "proposal": { "description": "ruling", "kind": kind } }))
        .deposit(NearToken::from_millinear(100))
        .gas(tgas(100))
        .transact()
        .await?;
    Ok(ok(r, "add_proposal").json()?)
}

async fn vote(
    m: &Account,
    dao: &AccountId,
    id: u64,
    kind: &Value,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(m.call(dao, "act_proposal")
        .args_json(json!({ "id": id, "action": "VoteApprove", "proposal": kind }))
        .gas(tgas(300))
        .transact()
        .await?)
}

async fn proposal_status(e: &Env, dao: &AccountId, id: u64) -> anyhow::Result<String> {
    let p: Value = e
        .worker
        .view(dao, "get_proposal")
        .args_json(json!({ "id": id }))
        .await?
        .json()?;
    Ok(p["status"].as_str().unwrap_or("?").to_string())
}

#[tokio::test]
async fn a_real_sputnik_dao_rules_once_and_only_while_it_is_the_arbiter() -> anyhow::Result<()> {
    let Some(dao_wasm) = optional_wasm("SPUTNIK_DAO_WASM") else {
        println!("SPUTNIK_DAO_WASM not set: skipped");
        return Ok(());
    };
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let m1 = sub(&e.root, "m1", 10).await?;
    let m2 = sub(&e.root, "m2", 10).await?;
    let m3 = sub(&e.root, "m3", 10).await?;
    let vote_policy = json!({ "weight_kind": "RoleWeight", "quorum": "0", "threshold": [1, 2] });
    let policy = json!({
        "roles": [{ "name": "council", "kind": { "Group": [m1.id(), m2.id(), m3.id()] },
            "permissions": ["*:AddProposal", "*:VoteApprove", "*:VoteReject", "*:VoteRemove", "*:Finalize"], "vote_policy": {} }],
        "default_vote_policy": vote_policy, "proposal_bond": "100000000000000000000000",
        "proposal_period": "604800000000000", "bounty_bond": "100000000000000000000000",
        "bounty_forgiveness_period": "86400000000000",
    });
    let dao_id: AccountId = format!("arb.{}", e.root.id()).parse()?;
    ok(
        e.root.batch(&dao_id).create_account().transfer(NearToken::from_near(10)).deploy(&dao_wasm)
            .call(Function::new("new").args_json(json!({ "config": { "name": "arb", "purpose": "rules", "metadata": "" }, "policy": policy })).gas(tgas(100)))
            .transact().await?,
        "deploy dao",
    );
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "set_arbiter",
            json!({ "arbiter": &dao_id }),
        )
        .await?,
        "set_arbiter dao",
    );

    // A ruling for the client, 2 of 3.
    let j1 = disputed_job(&e, client, worker, 1_000_000).await?;
    let k = ruling_kind(&pid, j1, false);
    let p = propose(&m1, &dao_id, &k).await?;
    ok(vote(&m1, &dao_id, p, &k).await?, "vote 1");
    assert_eq!(
        job(&e, j1).await?["state"],
        "Disputed",
        "one vote is not a ruling"
    );
    let r = ok(vote(&m2, &dao_id, p, &k).await?, "vote 2");
    assert_eq!(job(&e, j1).await?["state"], "Refunded");
    let evs = events(&r, &pid);
    assert!(names(&evs).contains(&"dispute_resolved".to_string()));
    assert_eq!(field(&evs, "dispute_resolved")["arbiter"], json!(dao_id));
    println!(
        "ruling proposal {p}: {}",
        proposal_status(&e, &dao_id, p).await?
    );
    check_i1(&e).await?;

    // Ruling twice through the DAO.
    let snap = totals(&e).await?;
    let k2 = ruling_kind(&pid, j1, true);
    let p2 = propose(&m1, &dao_id, &k2).await?;
    vote(&m1, &dao_id, p2, &k2).await?;
    let r = vote(&m2, &dao_id, p2, &k2).await?;
    assert!(
        events(&r, &pid).is_empty(),
        "no event from the failed second ruling"
    );
    assert_eq!(job(&e, j1).await?["state"], "Refunded");
    assert_eq!(totals(&e).await?, snap);
    println!(
        "second ruling on job {j1}: vote tx ok={} proposal {}",
        r.is_success(),
        proposal_status(&e, &dao_id, p2).await?
    );

    // A ruling on a job that is not disputed.
    let j2 = create_job(&e, client, worker, 100_000, 300, 1800).await?;
    let k3 = ruling_kind(&pid, j2, true);
    let p3 = propose(&m2, &dao_id, &k3).await?;
    vote(&m2, &dao_id, p3, &k3).await?;
    vote(&m3, &dao_id, p3, &k3).await?;
    assert_eq!(job(&e, j2).await?["state"], "Created");
    println!(
        "ruling on a Created job: proposal {}",
        proposal_status(&e, &dao_id, p3).await?
    );

    // The owner changes the arbiter while a ruling is being voted.
    let j3 = disputed_job(&e, client, worker, 1_000_000).await?;
    let k4 = ruling_kind(&pid, j3, true);
    let p4 = propose(&m1, &dao_id, &k4).await?;
    vote(&m1, &dao_id, p4, &k4).await?;
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "set_arbiter",
            json!({ "arbiter": e.dao.id() }),
        )
        .await?,
        "set_arbiter mid-dispute",
    );
    let r = vote(&m3, &dao_id, p4, &k4).await?;
    assert!(
        format!("{r:?}").contains("NotArbiter"),
        "the DAO is no longer the arbiter: {r:?}"
    );
    assert_eq!(job(&e, j3).await?["state"], "Disputed");
    ok(
        call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j3, "for_worker": false }),
        )
        .await?,
        "new arbiter rules",
    );
    assert_eq!(job(&e, j3).await?["state"], "Refunded");

    // A ruling that lands after expire_dispute.
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "set_arbiter",
            json!({ "arbiter": &dao_id }),
        )
        .await?,
        "dao again",
    );
    let j4 = disputed_job(&e, client, worker, 1_000_000).await?;
    let k5 = ruling_kind(&pid, j4, false);
    let p5 = propose(&m1, &dao_id, &k5).await?;
    vote(&m1, &dao_id, p5, &k5).await?;
    let dl = n(&job(&e, j4).await?["dispute_deadline"]) as u64;
    wait_past(&e, dl).await?;
    refused(
        &call(&e.ww, &pid, "expire_dispute", json!({ "job_id": j4 })).await?,
        "DeadlineNotPassed",
        "worker expires inside the grace",
    );
    wait_past(&e, dl + grace(&e).await?).await?;
    ok(
        call(&e.ww, &pid, "expire_dispute", json!({ "job_id": j4 })).await?,
        "worker expires the dispute after the grace",
    );
    let r = vote(&m2, &dao_id, p5, &k5).await?;
    assert!(format!("{r:?}").contains("InvalidState"));
    let jj = job(&e, j4).await?;
    assert_eq!(jj["state"], "Settled");
    assert_eq!(jj["outcome"], "Unresolved");
    println!(
        "late DAO ruling for the client after the worker expired the dispute: proposal {}",
        proposal_status(&e, &dao_id, p5).await?
    );
    check_i1(&e).await?;
    Ok(())
}

async fn snapshot(e: &Env, ids: &[u64], agents: &[u64]) -> anyhow::Result<Vec<Value>> {
    let mut v = vec![];
    for id in ids {
        v.push(job(e, *id).await?);
    }
    for a in agents {
        let a = *a;
        v.push(stake_of(e, a).await?);
        v.push(view(e, "get_agent", json!({ "agent_id": a })).await?);
        v.push(view(e, "worker_record", json!({ "agent_id": a })).await?);
        v.push(view(e, "client_record", json!({ "agent_id": a })).await?);
        v.push(json!(balance_of(e, a).await?.to_string()));
        v.push(json!(storage_bal(e, a).await?.to_string()));
    }
    v.push(totals(e).await?);
    v.push(view(e, "get_roles", json!({})).await?);
    Ok(v)
}

async fn sum_tokens(e: &Env, accts: &[AccountId]) -> anyhow::Result<u128> {
    let mut s = 0;
    for a in accts {
        s += ft_balance(e, a).await?;
    }
    Ok(s)
}

// ═══════════════════════════ 11. upgrade over live state ═══════════════════════════

#[tokio::test]
async fn upgrading_from_the_old_wasm_keeps_every_job_settleable() -> anyhow::Result<()> {
    let Some(old) = optional_wasm("OLD_PACTORA_WASM") else {
        println!("OLD_PACTORA_WASM not set: skipped");
        return Ok(());
    };
    let new = wasm_from("PACTORA_WASM", "./").await?;
    let e = setup_with(&old).await?;
    let pid = e.pactora.id().clone();
    let (client, worker) = market_on(&e).await?;
    let created = create_job(&e, client, worker, 1_000_000, 3000, 1800).await?;
    let accepted = create_job(&e, client, worker, 1_000_000, 300, 1800).await?;
    accept(&e, accepted).await?;
    let submitted = submitted_job(&e, client, worker, 1_000_000).await?;
    let disputed = disputed_job(&e, client, worker, 1_000_000).await?;
    let settled = submitted_job(&e, client, worker, 1_000_000).await?;
    ok(
        call(&e.cw, &pid, "approve", json!({ "job_id": settled })).await?,
        "approve",
    );
    let refunded = create_job(&e, client, worker, 1_000_000, 300, 1800).await?;
    ok(
        call(&e.cw, &pid, "cancel", json!({ "job_id": refunded })).await?,
        "cancel",
    );
    let direct: u64 = ok(call(&e.cw, &pid, "direct_pay", json!({ "client_id": client, "worker_id": worker, "amount": "20000", "spec_hash": SPEC })).await?, "direct").json()?;
    ok(
        call1(
            &e.bob,
            &pid,
            "request_unstake",
            json!({ "agent_id": worker, "amount": "1000000" }),
        )
        .await?,
        "request_unstake",
    );

    let ids = [
        created, accepted, submitted, disputed, settled, refunded, direct,
    ];
    let before = snapshot(&e, &ids, &[client, worker]).await?;
    let params_before = view(&e, "get_params", json!({})).await?;
    // Deploy and migrate in one batch, as the contract account.
    let d = e
        .pactora
        .as_account()
        .batch(&pid)
        .deploy(&new)
        .call(Function::new("migrate").gas(tgas(50)))
        .transact()
        .await?;
    assert!(d.is_success(), "redeploy + migrate: {d:?}");
    let after = snapshot(&e, &ids, &[client, worker]).await?;
    assert_eq!(before, after, "state reads the same through the new code");
    let mut params_after = view(&e, "get_params", json!({})).await?;
    assert_eq!(
        params_after["dispute_grace"], 86_400,
        "a migrated state gets the default grace"
    );
    params_after
        .as_object_mut()
        .unwrap()
        .remove("dispute_grace");
    assert_eq!(
        params_before, params_after,
        "every other parameter is unchanged"
    );
    assert_eq!(view(&e, "version", json!({})).await?, 1);
    // The owner shortens the grace so the test can wait it out.
    ok(
        call1(
            e.pactora.as_account(),
            &pid,
            "set_params",
            json!({ "params": params() }),
        )
        .await?,
        "set_params after migrate",
    );
    check_i1(&e).await?;

    // Finish every open job on the new code.
    accept(&e, created).await?;
    submit(&e, created).await?;
    ok(
        call(&e.cw, &pid, "approve", json!({ "job_id": created })).await?,
        "approve created",
    );
    submit(&e, accepted).await?;
    wait_past(&e, n(&job(&e, accepted).await?["review_deadline"]) as u64).await?;
    ok(
        call(
            &e.alice,
            &pid,
            "auto_approve",
            json!({ "job_id": accepted }),
        )
        .await?,
        "auto_approve",
    );
    let r = call(&e.cw, &pid, "dispute", json!({ "job_id": submitted })).await?;
    println!(
        "dispute of a pre-upgrade Submitted job: ok={}",
        r.is_success()
    );
    if job(&e, submitted).await?["state"] == "Disputed" {
        ok(
            call(
                &e.dao,
                &pid,
                "resolve_dispute",
                json!({ "job_id": submitted, "for_worker": false }),
            )
            .await?,
            "resolve",
        );
    } else {
        ok(
            call(
                &e.alice,
                &pid,
                "auto_approve",
                json!({ "job_id": submitted }),
            )
            .await?,
            "auto_approve submitted",
        );
    }
    wait_past(
        &e,
        n(&job(&e, disputed).await?["dispute_deadline"]) as u64 + grace(&e).await?,
    )
    .await?;
    ok(
        call(
            &e.alice,
            &pid,
            "expire_dispute",
            json!({ "job_id": disputed }),
        )
        .await?,
        "expire_dispute",
    );
    for id in ids {
        let s = job(&e, id).await?["state"].as_str().unwrap().to_string();
        assert!(s == "Settled" || s == "Refunded", "job {id} still {s}");
    }
    wait_past(&e, n(&stake_of(&e, worker).await?["unlock_at"]) as u64).await?;
    ok(
        e.bob
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": worker }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "complete_unstake",
    );
    ok(
        e.alice
            .call(&pid, "claim_fees")
            .max_gas()
            .transact()
            .await?,
        "claim_fees",
    );
    check_i1(&e).await?;
    let t = totals(&e).await?;
    assert_eq!(t["locked"], "0");
    assert_eq!(t["dispute_bonds"], "0");
    assert_eq!(n(&stake_of(&e, worker).await?["locked"]), 0);
    Ok(())
}

// ═══════════════════════════ 12. events ═══════════════════════════

#[tokio::test]
async fn every_state_change_emits_what_the_indexer_reads() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let mut seen = std::collections::BTreeSet::new();
    let mut take = |r: &ExecutionFinalResult, want: &[&str]| -> Vec<Value> {
        let evs = events(r, &pid);
        for ev in &evs {
            check_indexer(ev);
            seen.insert(ev["event"].as_str().unwrap().to_string());
        }
        let got = names(&evs);
        for w in want {
            assert_eq!(
                got.iter().filter(|g| g.as_str() == *w).count(),
                1,
                "exactly one {w} in {got:?}"
            );
        }
        evs
    };
    let r = send_in(
        &e,
        &e.alice,
        1_000,
        json!({ "Fund": { "agent_id": client } }),
    )
    .await?;
    take(&r, &["funded"]);

    // created → accepted → submitted → approved
    let r = ok(call(&e.cw, &pid, "create_job", json!({ "client_id": client, "worker_id": worker, "amount": "1000000", "spec_hash": SPEC, "accept_window": 300, "work_window": 1800 })).await?, "create");
    let evs = take(&r, &["job_created"]);
    let j: u64 = r.json()?;
    let jj = job(&e, j).await?;
    let d = field(&evs, "job_created");
    assert_eq!(d["amount"], jj["amount"]);
    assert_eq!(d["fee"], jj["fee"]);
    assert_eq!(d["accept_deadline"], jj["accept_deadline"]);
    assert_eq!(d["work_deadline"], jj["work_deadline"]);
    let evs = take(&accept(&e, j).await?, &["job_accepted"]);
    assert_eq!(
        field(&evs, "job_accepted")["bond"],
        job(&e, j).await?["worker_bond"]
    );
    let evs = take(&submit(&e, j).await?, &["result_submitted"]);
    assert_eq!(
        field(&evs, "result_submitted")["review_deadline"],
        job(&e, j).await?["review_deadline"]
    );
    let evs = take(
        &ok(
            call(&e.cw, &pid, "approve", json!({ "job_id": j })).await?,
            "approve",
        ),
        &["job_settled"],
    );
    assert_eq!(field(&evs, "job_settled")["outcome"], "Success");
    assert_eq!(n(&field(&evs, "job_settled")["paid"]), 990_000);

    // dispute without a reason, ruled for the worker
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    let evs = take(
        &ok(
            call(&e.cw, &pid, "dispute", json!({ "job_id": j })).await?,
            "dispute",
        ),
        &["job_disputed"],
    );
    assert!(field(&evs, "job_disputed")["reason_hash"].is_null());
    take(
        &ok(
            call(
                &e.dao,
                &pid,
                "resolve_dispute",
                json!({ "job_id": j, "for_worker": true }),
            )
            .await?,
            "rule w",
        ),
        &["dispute_resolved", "dispute_bond_settled", "job_settled"],
    );
    // dispute with a reason, ruled for the client
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    let evs = take(&dispute(&e, j).await?, &["job_disputed"]);
    assert_eq!(field(&evs, "job_disputed")["reason_hash"], REASON);
    let evs = take(
        &ok(
            call(
                &e.dao,
                &pid,
                "resolve_dispute",
                json!({ "job_id": j, "for_worker": false }),
            )
            .await?,
            "rule c",
        ),
        &["dispute_resolved", "dispute_bond_settled", "job_refunded"],
    );
    assert_eq!(field(&evs, "job_refunded")["outcome"], "DisputeLost");
    assert_eq!(field(&evs, "job_refunded")["reason"], "dispute");
    // expired dispute: half the bond each way, so two dispute_bond_settled
    let j = disputed_job(&e, client, worker, 1_000_000).await?;
    wait_past(
        &e,
        n(&job(&e, j).await?["dispute_deadline"]) as u64 + grace(&e).await?,
    )
    .await?;
    let evs = take(
        &ok(
            call(&e.alice, &pid, "expire_dispute", json!({ "job_id": j })).await?,
            "expire_dispute",
        ),
        &["dispute_expired", "job_settled"],
    );
    assert_eq!(field(&evs, "job_settled")["outcome"], "Unresolved");
    assert_eq!(
        names(&evs)
            .iter()
            .filter(|x| *x == "dispute_bond_settled")
            .count(),
        2,
        "{evs:?}"
    );
    let d = field(&evs, "dispute_expired");
    assert_eq!(
        (d["bond_to_worker"].as_str(), d["bond_to_client"].as_str()),
        (Some("50000"), Some("50000")),
        "{d}"
    );
    let settled: Vec<(u64, &str)> = evs
        .iter()
        .filter(|e| e["event"] == "dispute_bond_settled")
        .map(|e| {
            (
                e["data"][0]["to"].as_u64().unwrap(),
                e["data"][0]["amount"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(settled, [(worker, "50000"), (client, "50000")]);
    // auto approve
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    wait_past(&e, n(&job(&e, j).await?["review_deadline"]) as u64).await?;
    take(
        &ok(
            call(&e.alice, &pid, "auto_approve", json!({ "job_id": j })).await?,
            "auto_approve",
        ),
        &["job_settled"],
    );
    // cancel, unaccepted, undelivered
    let j = create_job(&e, client, worker, 1_000_000, 300, 1800).await?;
    let evs = take(
        &ok(
            call(&e.cw, &pid, "cancel", json!({ "job_id": j })).await?,
            "cancel",
        ),
        &["job_refunded"],
    );
    assert_eq!(field(&evs, "job_refunded")["reason"], "cancelled");
    assert!(field(&evs, "job_refunded")["outcome"].is_null());
    let j = create_job(&e, client, worker, 1_000_000, 1, 1).await?;
    wait_past(&e, n(&job(&e, j).await?["accept_deadline"]) as u64).await?;
    let evs = take(
        &ok(
            call(&e.alice, &pid, "expire_unaccepted", json!({ "job_id": j })).await?,
            "unaccepted",
        ),
        &["job_refunded"],
    );
    assert_eq!(field(&evs, "job_refunded")["reason"], "unaccepted");
    let j = create_job(&e, client, worker, 1_000_000, 300, 1).await?;
    accept(&e, j).await?;
    wait_past(&e, n(&job(&e, j).await?["work_deadline"]) as u64).await?;
    let evs = take(
        &ok(
            call(&e.alice, &pid, "expire_undelivered", json!({ "job_id": j })).await?,
            "undelivered",
        ),
        &["job_refunded", "bond_seized"],
    );
    assert_eq!(field(&evs, "job_refunded")["outcome"], "Failed");
    assert_eq!(field(&evs, "bond_seized")["amount"], "200000");
    // direct pay
    take(&ok(call(&e.cw, &pid, "direct_pay", json!({ "client_id": client, "worker_id": worker, "amount": "20000", "spec_hash": SPEC })).await?, "direct"), &["direct_paid", "job_settled"]);

    for ev in [
        "job_created",
        "job_accepted",
        "result_submitted",
        "job_disputed",
        "dispute_resolved",
        "dispute_expired",
        "job_settled",
        "job_refunded",
        "bond_seized",
        "dispute_bond_settled",
        "direct_paid",
    ] {
        assert!(seen.contains(ev), "flow never emitted {ev}");
    }

    // Refusals: no event in a successful receipt.
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    for r in [
        call(&e.ww, &pid, "approve", json!({ "job_id": j })).await?,
        call(&e.cw, &pid, "accept_job", json!({ "job_id": j })).await?,
        call(&e.alice, &pid, "auto_approve", json!({ "job_id": j })).await?,
        call(
            &e.alice,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": true }),
        )
        .await?,
    ] {
        assert!(r.is_failure());
        assert!(
            events(&r, &pid).is_empty(),
            "event from a refused call: {r:?}"
        );
    }
    // A refusal AFTER the event was logged: create_job whose storage charge fails.
    let sb = storage_bal(&e, client).await?;
    ok(
        call1(
            &e.alice,
            &pid,
            "storage_withdraw",
            json!({ "agent_id": client, "amount": sb.to_string() }),
        )
        .await?,
        "drain storage",
    );
    let r = call(&e.cw, &pid, "create_job", json!({ "client_id": client, "worker_id": worker, "amount": "1000000", "spec_hash": SPEC, "accept_window": 300, "work_window": 1800 })).await?;
    refused(&r, "InsufficientStorage", "create_job without storage");
    assert!(events(&r, &pid).is_empty());
    println!(
        "logs kept in the failed create_job receipt (indexer must drop them): {:?}",
        failed_logs(&r)
    );
    Ok(())
}

// ═══════════════════════════ 13. fund accounting ═══════════════════════════

#[tokio::test]
async fn every_flow_balances_to_the_last_unit_and_nothing_stays_behind() -> anyhow::Result<()> {
    let e = setup().await?;
    let pid = e.pactora.id().clone();
    let supply: u128 = e
        .usdc
        .view("ft_total_supply")
        .await?
        .json::<String>()?
        .parse()?;
    let accts = [
        e.alice.id().clone(),
        e.bob.id().clone(),
        e.fees.id().clone(),
        pid.clone(),
        e.usdc.id().clone(),
    ];
    assert_eq!(sum_tokens(&e, &accts).await?, supply);
    let near0: Vec<u128> = {
        let mut v = vec![];
        for a in [e.alice.id(), e.bob.id(), &pid] {
            v.push(near_of(&e, a).await?);
        }
        v
    };
    let own0 = contract_own_near(&e).await?;

    let (client, worker) = market_on(&e).await?;
    let (mut c, mut w, mut fees, mut stake) = (20_000_000u128, 0u128, 0u128, 20_000_000u128);
    let amt = 1_000_000u128;
    let fee = 10_000u128;
    let bond = 200_000u128;
    let dbond = 100_000u128;

    // approve
    let j = submitted_job(&e, client, worker, amt).await?;
    ok(
        call(&e.cw, &pid, "approve", json!({ "job_id": j })).await?,
        "approve",
    );
    c -= amt;
    w += amt - fee;
    fees += fee;
    // dispute → worker
    let j = disputed_job(&e, client, worker, amt).await?;
    ok(
        call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": true }),
        )
        .await?,
        "rule w",
    );
    c -= amt + dbond;
    w += amt - fee + dbond;
    fees += fee;
    // dispute → client
    let j = disputed_job(&e, client, worker, amt).await?;
    ok(
        call(
            &e.dao,
            &pid,
            "resolve_dispute",
            json!({ "job_id": j, "for_worker": false }),
        )
        .await?,
        "rule c",
    );
    // client back whole (amount and bond); no fee
    // timeout: auto_approve
    let j = submitted_job(&e, client, worker, amt).await?;
    wait_past(&e, n(&job(&e, j).await?["review_deadline"]) as u64).await?;
    ok(
        call(&e.alice, &pid, "auto_approve", json!({ "job_id": j })).await?,
        "auto",
    );
    c -= amt;
    w += amt - fee;
    fees += fee;
    // dispute expires: the worker is paid and gets half the bond, the client the other half
    let j = disputed_job(&e, client, worker, amt).await?;
    wait_past(
        &e,
        n(&job(&e, j).await?["dispute_deadline"]) as u64 + grace(&e).await?,
    )
    .await?;
    ok(
        call(&e.alice, &pid, "expire_dispute", json!({ "job_id": j })).await?,
        "expire",
    );
    c -= amt + dbond / 2;
    w += amt - fee + dbond / 2;
    fees += fee;
    // cancel, unaccepted
    let j = create_job(&e, client, worker, amt, 300, 1800).await?;
    ok(
        call(&e.cw, &pid, "cancel", json!({ "job_id": j })).await?,
        "cancel",
    );
    let j = create_job(&e, client, worker, amt, 1, 1).await?;
    wait_past(&e, n(&job(&e, j).await?["accept_deadline"]) as u64).await?;
    ok(
        call(&e.alice, &pid, "expire_unaccepted", json!({ "job_id": j })).await?,
        "unaccepted",
    );
    // undelivered: bond seized to client
    let j = create_job(&e, client, worker, amt, 300, 1).await?;
    accept(&e, j).await?;
    wait_past(&e, n(&job(&e, j).await?["work_deadline"]) as u64).await?;
    ok(
        call(&e.alice, &pid, "expire_undelivered", json!({ "job_id": j })).await?,
        "undelivered",
    );
    c += bond;
    stake -= bond;
    // direct pay
    ok(call(&e.cw, &pid, "direct_pay", json!({ "client_id": client, "worker_id": worker, "amount": "20000", "spec_hash": SPEC })).await?, "direct");
    c -= 20_000;
    w += 20_000 - 200;
    fees += 200;

    assert_eq!(balance_of(&e, client).await?, c, "client purse");
    assert_eq!(balance_of(&e, worker).await?, w, "worker purse");
    assert_eq!(
        n(&stake_of(&e, worker).await?["available"]),
        stake,
        "worker stake"
    );
    assert_eq!(n(&totals(&e).await?["fees_owed"]), fees, "fees owed");
    let t = totals(&e).await?;
    assert_eq!(t["locked"], "0");
    assert_eq!(t["dispute_bonds"], "0");
    check_i1(&e).await?;

    // Everything out.
    ok(
        withdraw(&e, &e.alice, client, c, 100, None).await?,
        "withdraw client",
    );
    ok(
        withdraw(&e, &e.bob, worker, w, 100, None).await?,
        "withdraw worker",
    );
    ok(
        e.alice
            .call(&pid, "claim_fees")
            .max_gas()
            .transact()
            .await?,
        "claim_fees",
    );
    ok(
        call1(
            &e.bob,
            &pid,
            "request_unstake",
            json!({ "agent_id": worker, "amount": stake.to_string() }),
        )
        .await?,
        "unstake all",
    );
    wait_past(&e, n(&stake_of(&e, worker).await?["unlock_at"]) as u64).await?;
    ok(
        e.bob
            .call(&pid, "complete_unstake")
            .args_json(json!({ "agent_id": worker }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "complete_unstake",
    );
    let t = totals(&e).await?;
    assert_eq!(t["sum"], "0", "every bucket empty: {t}");
    assert_eq!(
        ft_balance(&e, &pid).await?,
        0,
        "no token left in the contract"
    );
    assert_eq!(ft_balance(&e, e.fees.id()).await?, fees);
    assert_eq!(
        ft_balance(&e, e.alice.id()).await?,
        100_000_000 - 20_000_000 + c
    );
    assert_eq!(
        ft_balance(&e, e.bob.id()).await?,
        100_000_000 - 20_000_000 + w + stake
    );
    assert_eq!(
        sum_tokens(&e, &accts).await?,
        supply,
        "token supply conserved"
    );
    assert_eq!(
        200_000_000u128,
        ft_balance(&e, e.alice.id()).await? + ft_balance(&e, e.bob.id()).await? + fees,
        "alice+bob+fees = 200M"
    );

    // NEAR: owners take their storage balances back; the contract never subsidised anyone.
    for (o, a) in [(&e.alice, client), (&e.bob, worker)] {
        let sb = storage_bal(&e, a).await?;
        ok(
            call1(
                o,
                &pid,
                "storage_withdraw",
                json!({ "agent_id": a, "amount": sb.to_string() }),
            )
            .await?,
            "storage back",
        );
    }
    let own1 = contract_own_near(&e).await?;
    assert!(own1 >= own0, "contract's own NEAR {own0} -> {own1}");
    let names = ["alice", "bob", "pactora"];
    for (i, a) in [e.alice.id(), e.bob.id(), &pid].iter().enumerate() {
        let d = near_of(&e, a).await? as i128 - near0[i] as i128;
        println!("NEAR delta {:<8} {:>+.6} NEAR", names[i], d as f64 / 1e24);
    }
    println!(
        "contract's own NEAR grew by {} yocto (gas rewards + 1-yocto deposits)",
        own1 - own0
    );
    Ok(())
}

// ═══════════════════════════ 14. surplus recovery ═══════════════════════════

/// Tokens a plain `ft_transfer` left in the contract are nobody's: the owner
/// may recover them, and only them — never a unit of what the protocol owes.
#[tokio::test]
async fn the_owner_recovers_only_the_surplus_above_the_totals() -> anyhow::Result<()> {
    let (e, client, worker) = market().await?;
    let pid = e.pactora.id().clone();
    let owner = e.pactora.as_account();
    let j = submitted_job(&e, client, worker, 1_000_000).await?;
    ok(
        call(&e.cw, &pid, "approve", json!({ "job_id": j })).await?,
        "approve",
    );
    check_i1(&e).await?;

    // Nothing above the totals yet: the callback refuses, nothing moves.
    let held0 = ft_balance(&e, &pid).await?;
    let r = owner
        .call(&pid, "recover_surplus")
        .args_json(json!({ "amount": "1", "receiver": e.alice.id() }))
        .deposit(yocto(1))
        .max_gas()
        .transact()
        .await?;
    refused(&r, "NoSurplus", "recover_surplus with no surplus");
    assert_eq!(ft_balance(&e, &pid).await?, held0);
    check_i1(&e).await?;

    // A plain ft_transfer: 777 units the protocol never credited.
    ok(
        e.alice
            .call(e.usdc.id(), "ft_transfer")
            .args_json(json!({ "receiver_id": &pid, "amount": "777" }))
            .deposit(yocto(1))
            .transact()
            .await?,
        "plain ft_transfer",
    );
    assert_eq!(
        ft_balance(&e, &pid).await? - n(&totals(&e).await?["sum"]),
        777
    );

    // Strangers, parties and the owner without its yocto are refused.
    let r = call1(
        &e.alice,
        &pid,
        "recover_surplus",
        json!({ "amount": "777", "receiver": e.alice.id() }),
    )
    .await?;
    refused(&r, "NotOwner", "a party recovering");
    let r = call(
        owner,
        &pid,
        "recover_surplus",
        json!({ "amount": "777", "receiver": e.alice.id() }),
    )
    .await?;
    refused(
        &r,
        "Requires attached deposit of exactly 1 yoctoNEAR",
        "owner without the yocto",
    );
    let r = owner
        .call(&pid, "recover_surplus")
        .args_json(json!({ "amount": "777", "receiver": e.alice.id() }))
        .deposit(yocto(1))
        .gas(tgas(60))
        .transact()
        .await?;
    refused(&r, "NotEnoughGas", "owner with too little gas");
    assert_eq!(
        ft_balance(&e, &pid).await? - n(&totals(&e).await?["sum"]),
        777,
        "nothing moved"
    );

    // Asking for more than the surplus yields exactly the surplus.
    let alice0 = ft_balance(&e, e.alice.id()).await?;
    let r = ok(
        owner
            .call(&pid, "recover_surplus")
            .args_json(json!({ "amount": "1000000", "receiver": e.alice.id() }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "recover_surplus",
    );
    let evs = events(&r, &pid);
    assert_eq!(
        names(&evs),
        ["surplus_recovered", "transfer_started", "transfer_finished"],
        "{evs:?}"
    );
    assert_eq!(field(&evs, "surplus_recovered")["amount"], "777");
    assert_eq!(field(&evs, "surplus_recovered")["surplus"], "777");
    assert_eq!(field(&evs, "transfer_finished")["ok"], true);
    assert_eq!(ft_balance(&e, e.alice.id()).await? - alice0, 777);
    assert_eq!(
        ft_balance(&e, &pid).await?,
        n(&totals(&e).await?["sum"]),
        "token balance == totals again"
    );
    check_i1(&e).await?;

    // A payout that fails leaves the surplus where it was, still recoverable.
    ok(
        e.alice
            .call(e.usdc.id(), "ft_transfer")
            .args_json(json!({ "receiver_id": &pid, "amount": "5" }))
            .deposit(yocto(1))
            .transact()
            .await?,
        "plain ft_transfer 5",
    );
    let unreg = sub(&e.root, "unreg2", 5).await?;
    let r = ok(
        owner
            .call(&pid, "recover_surplus")
            .args_json(json!({ "amount": "5", "receiver": unreg.id() }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "recover_surplus to an unregistered receiver",
    );
    assert!(!r.json::<bool>()?, "the transfer failed");
    assert_eq!(
        ft_balance(&e, &pid).await? - n(&totals(&e).await?["sum"]),
        5,
        "still a surplus of 5"
    );
    assert_eq!(totals(&e).await?["in_flight"], "0");
    let r = ok(
        owner
            .call(&pid, "recover_surplus")
            .args_json(json!({ "amount": "5", "receiver": e.alice.id() }))
            .deposit(yocto(1))
            .max_gas()
            .transact()
            .await?,
        "recover_surplus again",
    );
    assert!(r.json::<bool>()?);
    check_i1(&e).await?;
    Ok(())
}
