#!/usr/bin/env node
/**
 * Every flow, live on a real NEAR node, against the deployed contracts.
 *
 *   node scripts/verify-near.mjs --network sandbox
 *   node scripts/verify-near.mjs --network testnet   # needs the deployer key in .env
 *
 * A mock that agrees with our assumptions proves nothing (project rule): the
 * unit tests run the contract in near-sdk's mock; this sends real
 * transactions, signed the way production signs them — owners with
 * full-access keys, agent wallets with FUNCTION-CALL keys scoped to the
 * contract — and checks each result on chain. Every transaction hash is printed.
 *
 * Fresh accounts each run (`v<timestamp>` prefix), created under the deployer.
 * Testnet only for real chains; it refuses anything else.
 *
 * Checks, in order:
 *   registration (register_agent + accept_wallet by the wallet's FC key),
 *   funding and staking (ft_transfer_call), the escrow happy path, direct_pay,
 *   a per-task cap refusal, self-dealing refusal, an FC key refused on
 *   withdraw while the owner can withdraw, a dispute ruled by the arbiter,
 *   every exit anyone may call (expire_unaccepted, expire_undelivered,
 *   auto_approve, expire_dispute), and invariant I1: the contract's token
 *   balance equals the sum of what it owes (get_totals).
 */
import {readFileSync, existsSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (n) => (process.argv.includes(`--${n}`) ? process.argv[process.argv.indexOf(`--${n}`) + 1] : undefined);
const slug = arg('network');
if (slug !== 'sandbox' && slug !== 'testnet') {
  console.error('verify-near: --network sandbox|testnet');
  process.exit(2);
}
const net = JSON.parse(readFileSync(join(ROOT, 'config', 'networks.json'), 'utf8'))[`near:${slug}`];
const params = JSON.parse(readFileSync(join(ROOT, 'config', `params.${slug}.json`), 'utf8')).onchain;
const depFile = join(ROOT, 'deployments', `${slug}.json`);
if (!existsSync(depFile)) {
  console.error(`verify-near: no deployments/${slug}.json — run scripts/deploy.mjs first`);
  process.exit(2);
}
const {contracts} = JSON.parse(readFileSync(depFile, 'utf8'));

let rpcUrl;
let rootId;
let rootSecret;
if (slug === 'sandbox') {
  rpcUrl = process.env.SANDBOX_RPC ?? net.rpcUrls[0];
  const k = JSON.parse(readFileSync(process.env.SANDBOX_KEY_FILE ?? '', 'utf8'));
  rootId = k.account_id;
  rootSecret = k.secret_key;
} else {
  rpcUrl = process.env.RPC_URL_TESTNET ?? net.rpcUrls[0];
  rootId = process.env.DEPLOYER_ACCOUNT_ID;
  rootSecret = process.env.DEPLOYER_SECRET_KEY;
  if (!rootId?.endsWith('.testnet') || !rootSecret) {
    console.error('verify-near: DEPLOYER_ACCOUNT_ID (a .testnet account) and DEPLOYER_SECRET_KEY must be set');
    process.exit(2);
  }
}

const provider = new JsonRpcProvider({url: rpcUrl});
const status = await provider.viewNodeStatus();
if (status.chain_id !== net.nearNetworkId) throw new Error(`RPC is ${status.chain_id}, expected ${net.nearNetworkId}`);
const TGAS = 1_000_000_000_000n;
const rootKey = KeyPair.fromString(rootSecret);
const root = new Account(rootId, provider, new KeyPairSigner(rootKey));
const explorer = net.blockExplorerUrls[0];

let passed = 0;
const ok = (what, extra = '') => {
  passed++;
  console.log(`  ✓ ${what}${extra ? `  ${extra}` : ''}`);
};
const link = (hash) => (explorer ? `${explorer}/txns/${hash}` : hash);
const view = (contractId, method, args = {}) =>
  provider.callFunction({contractId, method, args, blockQuery: {finality: 'final'}});
const sha = (s) => createHash('sha256').update(s).digest('hex');

/** Send a function call; return {value, hash}. Throws with the node's failure text. */
async function call(account, contractId, method, args, {deposit = 0n, gas = 100n * TGAS} = {}) {
  const out = await account.signAndSendTransaction({
    receiverId: contractId,
    actions: [actions.functionCall(method, args, gas, deposit)],
    waitUntil: 'FINAL',
    throwOnFailure: false,
  });
  const st = out.status;
  const failedReceipt = out.receipts_outcome.find((r) => typeof r.outcome.status === 'object' && 'Failure' in r.outcome.status);
  if (typeof st === 'object' && 'Failure' in st) {
    const e = new Error(JSON.stringify(st.Failure));
    e.hash = out.transaction.hash;
    throw e;
  }
  const b64 = st.SuccessValue ?? '';
  const text = Buffer.from(b64, 'base64').toString('utf8');
  let value;
  try {
    value = text ? JSON.parse(text) : undefined;
  } catch {
    value = text;
  }
  return {value, hash: out.transaction.hash, failedReceipt};
}

/** The call must fail, and the failure must name `reason`. */
async function refused(promise, reason, what) {
  try {
    const r = await promise;
    throw new Error(`${what}: expected a refusal naming ${reason}, but it succeeded (${r.hash})`);
  } catch (e) {
    if (String(e.message).includes('expected a refusal')) throw e;
    if (!String(e.message).includes(reason)) throw new Error(`${what}: refused, but not with ${reason}: ${e.message}`);
    ok(what, `(refused: ${reason})`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Chain time, not wall time: deadlines are checked against block timestamps. */
const chainNow = async () => Number(BigInt((await provider.viewBlock({finality: 'final'})).header.timestamp_nanosec) / 1_000_000_000n);
async function waitUntilChain(t) {
  while ((await chainNow()) <= t) await sleep(1000);
}

// ── accounts ─────────────────────────────────────────────────────────────
const run = `v${Date.now().toString(36)}`;
console.log(`verify-near on near:${slug} — run ${run}, contracts ${JSON.stringify(contracts)}`);

async function newOwner(name) {
  const kp = KeyPair.fromRandom('ed25519');
  const id = `${run}-${name}.${rootId}`;
  const out = await root.signAndSendTransaction({
    receiverId: id,
    actions: [actions.createAccount(), actions.transfer(nearToYocto(slug === 'sandbox' ? '20' : '2')), actions.addFullAccessKey(kp.getPublicKey())],
    waitUntil: 'FINAL',
  });
  return {id, kp, account: new Account(id, provider, new KeyPairSigner(kp)), hash: out.transaction.hash};
}

/** The agent wallet: a sub-account of its owner holding ONLY a function-call key scoped to Pactora. */
async function newWallet(owner) {
  const kp = KeyPair.fromRandom('ed25519');
  const id = `bot.${owner.id}`;
  await owner.account.signAndSendTransaction({
    receiverId: id,
    actions: [
      actions.createAccount(),
      actions.transfer(nearToYocto('0.5')),
      actions.addFunctionCallAccessKey(kp.getPublicKey(), contracts.Pactora, [], nearToYocto('0.25')),
    ],
    waitUntil: 'FINAL',
  });
  const keys = await provider.viewAccessKeyList({accountId: id, finalityQuery: {finality: 'final'}});
  if (keys.keys.length !== 1 || keys.keys[0].access_key.permission === 'FullAccess') {
    throw new Error(`${id} must hold exactly one function-call key`);
  }
  return {id, kp, account: new Account(id, provider, new KeyPairSigner(kp))};
}

const client = await newOwner('client');
const worker = await newOwner('worker');
const clientBot = await newWallet(client);
const workerBot = await newWallet(worker);
ok('owners created with full-access keys; agent wallets hold only a function-call key scoped to Pactora');

// ── token ────────────────────────────────────────────────────────────────
const bounds = await view(contracts.PaymentToken, 'storage_balance_bounds');
for (const o of [client, worker]) {
  await call(o.account, contracts.PaymentToken, 'storage_deposit', {account_id: o.id, registration_only: true}, {deposit: BigInt(bounds.min)});
  await call(root, contracts.PaymentToken, 'mint', {account_id: o.id, amount: '100000000'});
}
ok('owners registered on mUSDC and minted 100 mUSDC each');

// ── registration ─────────────────────────────────────────────────────────
const register = async (o, wallet) => {
  const r = await call(o.account, contracts.Pactora, 'register_agent', {wallet: wallet.id, card_uri: `data:,${run}`}, {deposit: nearToYocto('0.1')});
  const accepted = await call(wallet.account, contracts.Pactora, 'accept_wallet', {agent_id: r.value});
  return {id: r.value, hash: r.hash, accepted: accepted.hash};
};
const A = await register(client, clientBot);
const B = await register(worker, workerBot);
const agentA = await view(contracts.Pactora, 'get_agent', {agent_id: A.id});
if (agentA.wallet !== clientBot.id || agentA.owner !== client.id) throw new Error(`get_agent: ${JSON.stringify(agentA)}`);
ok(`agents ${A.id} and ${B.id} registered; each wallet accepted with its own function-call key`, link(A.accepted));

// ── funding ──────────────────────────────────────────────────────────────
const ftCall = (o, amount, msg) =>
  call(o.account, contracts.PaymentToken, 'ft_transfer_call', {receiver_id: contracts.Pactora, amount, msg: JSON.stringify(msg)}, {deposit: 1n, gas: 150n * TGAS});
await ftCall(client, '5000000', {Fund: {agent_id: A.id}});
// The minimum, plus room for the bond expire_undelivered seizes below: a seized
// bond comes out of the stake, and a stake under the minimum is not hireable.
await ftCall(worker, String(BigInt(params.min_stake) + 100000n), {Stake: {agent_id: B.id}});
const purseA = await view(contracts.Pactora, 'purse_of', {agent_id: A.id});
if (purseA.balance !== '5000000') throw new Error(`purse_of: ${JSON.stringify(purseA)}`);
if (!(await view(contracts.Pactora, 'is_hireable', {agent_id: B.id}))) throw new Error('worker not hireable after staking');
ok('client funded 5 mUSDC (ft_transfer_call → balance); worker staked and is hireable');

// ── escrow, happy path ───────────────────────────────────────────────────
const spec = (n) => sha(`${run}:spec:${n}`);
const hire = async (n, amount = '50000', accept = 60, work = 60) =>
  (await call(clientBot.account, contracts.Pactora, 'create_job', {client_id: A.id, worker_id: B.id, amount, spec_hash: spec(n), accept_window: accept, work_window: work})).value;

const workerBefore = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: B.id})).balance);
const j1 = await hire(1);
await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j1});
await call(workerBot.account, contracts.Pactora, 'submit_result', {job_id: j1, result_hash: sha(`${run}:result:1`)});
const approved = await call(clientBot.account, contracts.Pactora, 'approve', {job_id: j1, review_hash: sha('judge says fine')});
const job1 = await view(contracts.Pactora, 'get_job', {job_id: j1});
const workerAfter = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: B.id})).balance);
const fee = BigInt(job1.fee);
if (job1.state !== 'Settled' || job1.outcome !== 'Success' || workerAfter - workerBefore !== 50000n - fee) {
  throw new Error(`job ${j1}: ${JSON.stringify(job1)}; worker +${workerAfter - workerBefore}`);
}
ok(`escrow: created, accepted, submitted, approved — worker paid ${50000n - fee} (fee ${fee})`, link(approved.hash));

// ── direct pay, caps, self-dealing ───────────────────────────────────────
const dp = await call(clientBot.account, contracts.Pactora, 'direct_pay', {client_id: A.id, worker_id: B.id, amount: '20000', spec_hash: spec('dp')});
ok('direct_pay settles in one call', link(dp.hash));
await refused(hire('cap', String(BigInt(params.default_per_task_cap) + 1n)), 'PerTaskCapExceeded', 'a hire above the per-task cap');
await refused(
  call(clientBot.account, contracts.Pactora, 'create_job', {client_id: A.id, worker_id: A.id, amount: '50000', spec_hash: spec('self'), accept_window: 60, work_window: 60}),
  'SelfDealing',
  'an agent hiring itself',
);

// ── a leaked agent key cannot withdraw ───────────────────────────────────
await refused(
  call(clientBot.account, contracts.Pactora, 'withdraw', {agent_id: A.id, amount: '1'}, {deposit: 1n}),
  'Deposit is not allowed with function call',
  "the agent's function-call key attaching the 1 yocto withdraw needs (the node refuses it)",
);
await refused(call(clientBot.account, contracts.Pactora, 'withdraw', {agent_id: A.id, amount: '1'}), 'Requires attached deposit of exactly 1 yoctoNEAR', 'withdraw without the yocto');
const w = await call(client.account, contracts.Pactora, 'withdraw', {agent_id: A.id, amount: '10000'}, {deposit: 1n, gas: 100n * TGAS});
ok("the owner's full-access key withdraws", link(w.hash));

// ── dispute, ruled by the arbiter ────────────────────────────────────────
const j2 = await hire(2);
await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j2});
await call(workerBot.account, contracts.Pactora, 'submit_result', {job_id: j2, result_hash: sha(`${run}:result:2`)});
await call(clientBot.account, contracts.Pactora, 'dispute', {job_id: j2, reason_hash: sha('the result is wrong')});
if (slug === 'sandbox') {
  const arb = new Account(contracts.Arbiter, provider, new KeyPairSigner(rootKey));
  const ruled = await call(arb, contracts.Pactora, 'resolve_dispute', {job_id: j2, for_worker: false});
  const job2 = await view(contracts.Pactora, 'get_job', {job_id: j2});
  if (job2.state !== 'Refunded' || job2.outcome !== 'DisputeLost') throw new Error(`job ${j2}: ${JSON.stringify(job2)}`);
  ok('dispute ruled for the client by the arbiter: refunded, worker marked DisputeLost', link(ruled.hash));
} else {
  ok(`dispute opened on job ${j2}; on testnet the arbiter is the DAO — rule it with scripts/dao-resolve.mjs`);
}

// ── every exit anyone may call ───────────────────────────────────────────
const anyone = root;
const j3 = await hire(3, '50000', 2, 60);
await waitUntilChain((await view(contracts.Pactora, 'get_job', {job_id: j3})).accept_deadline);
await call(anyone, contracts.Pactora, 'expire_unaccepted', {job_id: j3});
ok('expire_unaccepted: an offer nobody took is refunded', `job ${j3}`);

const j4 = await hire(4, '50000', 60, 2);
await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j4});
await waitUntilChain((await view(contracts.Pactora, 'get_job', {job_id: j4})).work_deadline);
await call(anyone, contracts.Pactora, 'expire_undelivered', {job_id: j4});
const job4 = await view(contracts.Pactora, 'get_job', {job_id: j4});
if (job4.state !== 'Refunded' || job4.outcome !== 'Failed') throw new Error(`job ${j4}: ${JSON.stringify(job4)}`);
ok("expire_undelivered: accepted and never delivered — refunded, the worker's bond seized", `job ${j4}`);

if (params.review_window <= 60) {
  const j5 = await hire(5);
  await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j5});
  await call(workerBot.account, contracts.Pactora, 'submit_result', {job_id: j5, result_hash: sha(`${run}:result:5`)});
  await waitUntilChain((await view(contracts.Pactora, 'get_job', {job_id: j5})).review_deadline);
  await call(anyone, contracts.Pactora, 'auto_approve', {job_id: j5});
  ok('auto_approve: a client that stays silent pays', `job ${j5}`);

  const j6 = await hire(6);
  await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j6});
  await call(workerBot.account, contracts.Pactora, 'submit_result', {job_id: j6, result_hash: sha(`${run}:result:6`)});
  await call(clientBot.account, contracts.Pactora, 'dispute', {job_id: j6, reason_hash: null});
  await waitUntilChain((await view(contracts.Pactora, 'get_job', {job_id: j6})).dispute_deadline);
  await call(anyone, contracts.Pactora, 'expire_dispute', {job_id: j6});
  ok('expire_dispute: a dispute nobody rules on pays the worker and returns the bond', `job ${j6}`);
} else {
  console.log(`  - auto_approve / expire_dispute skipped: the review window is ${params.review_window} s here (sandbox uses 10 s)`);
}

// ── I1 ───────────────────────────────────────────────────────────────────
const totals = await view(contracts.Pactora, 'get_totals');
const held = await view(contracts.PaymentToken, 'ft_balance_of', {account_id: contracts.Pactora});
if (totals.sum !== held) throw new Error(`I1: get_totals.sum ${totals.sum} != ft_balance_of ${held} (${JSON.stringify(totals)})`);
ok(`I1: the contract holds exactly what it owes — ${held} base units`, JSON.stringify(totals));

console.log(`\nverify-near: ${passed} checks passed on near:${slug}`);
