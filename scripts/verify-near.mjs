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
 * On testnet each account's key is saved under .keys/testnet/ before the
 * account is created, and the full-access ones are deleted back into the
 * deployer at the end (and at the start, for a run that stopped half-way).
 * Testnet only for real chains; it refuses anything else.
 *
 * Checks, in order:
 *   registration (register_agent + accept_wallet by the wallet's FC key),
 *   funding and staking (ft_transfer_call), the escrow happy path, direct_pay,
 *   a per-task cap refusal, self-dealing refusal, an FC key refused on
 *   withdraw while the owner can withdraw, a stolen worker FC key refused
 *   locking a bond beyond its caps, a dispute ruled by the arbiter,
 *   every exit anyone may call (expire_unaccepted, expire_undelivered,
 *   auto_approve, expire_dispute — refused inside the grace after the
 *   deadline, then splitting the bond), a PERSON (one full-access account
 *   that is both owner and wallet) registering, staking, being hired,
 *   delivering and being paid, with SameOwner refused against its own
 *   agents, the owner recovering a surplus left by a plain ft_transfer (and
 *   only that), and invariant I1: the contract's token balance equals the
 *   sum of what it owes (get_totals).
 */
import {readFileSync, existsSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';
import {deployDaoSandbox, proposeRuling, vote} from './lib/dao.mjs';
import {saveKey, sweep} from './lib/throwaway.mjs';

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
  rpcUrl = process.env.SANDBOX_RPC || net.rpcUrls[0];
  const k = JSON.parse(readFileSync(process.env.SANDBOX_KEY_FILE || '', 'utf8'));
  rootId = k.account_id;
  rootSecret = k.secret_key;
} else {
  rpcUrl = process.env.RPC_URL_TESTNET || net.rpcUrls[0];
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

/** Testnet: the two council members besides the deployer (scripts/create-dao.mjs wrote them to .env). */
function councilFromEnv() {
  return [1, 2].map((i) => {
    const id = process.env[`COUNCIL_${i}_ACCOUNT_ID`];
    const secret = process.env[`COUNCIL_${i}_SECRET_KEY`];
    if (!id || !secret) throw new Error(`COUNCIL_${i}_ACCOUNT_ID and COUNCIL_${i}_SECRET_KEY must be set (scripts/create-dao.mjs)`);
    return new Account(id, provider, new KeyPairSigner(KeyPair.fromString(secret)));
  });
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
await sweep(ROOT, slug, provider, rootId);

async function newOwner(name) {
  const kp = KeyPair.fromRandom('ed25519');
  const id = `${run}-${name}.${rootId}`;
  saveKey(ROOT, slug, id, kp, {fullAccess: true});
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
  saveKey(ROOT, slug, id, kp, {fullAccess: false});
  await owner.account.signAndSendTransaction({
    receiverId: id,
    actions: [
      actions.createAccount(),
      // 0.2 NEAR, not more: no key can ever take back what a function-call-only
      // account holds. A call reserves its 100 TGas at the pessimistic gas price
      // up front (about 0.101 NEAR, so 0.1 is refused); a full run burns under 0.004.
      actions.transfer(nearToYocto('0.2')),
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

// ── a leaked WORKER key cannot hand its stake away as bonds ──────────────
// The attacker registers its own client agent with high caps and hires the
// victim for a large job. If the stolen worker wallet could accept it, the
// attacker would let it expire undelivered and collect the seized bond.
// Bonds count toward the worker's own caps, so this is refused.
const attacker = await newOwner('attacker');
const attackerBot = await newWallet(attacker);
await call(attacker.account, contracts.PaymentToken, 'storage_deposit', {account_id: attacker.id, registration_only: true}, {deposit: BigInt(bounds.min)});
await call(root, contracts.PaymentToken, 'mint', {account_id: attacker.id, amount: '10000000'});
const X = await register(attacker, attackerBot);
await call(attacker.account, contracts.Pactora, 'set_caps', {agent_id: X.id, per_task_cap: '5000000', daily_cap: '5000000'}, {deposit: 1n});
await ftCall(attacker, '2000000', {Fund: {agent_id: X.id}});
const jb = (await call(attackerBot.account, contracts.Pactora, 'create_job', {client_id: X.id, worker_id: B.id, amount: '1000000', spec_hash: spec('bond'), accept_window: 60, work_window: 60})).value;
const bondOf = BigInt((await view(contracts.Pactora, 'get_job', {job_id: jb})).worker_bond);
const workerCap = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: B.id})).per_task_cap);
if (bondOf <= workerCap) throw new Error(`setup: bond ${bondOf} must exceed the worker's per-task cap ${workerCap}`);
await refused(call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: jb}), 'PerTaskCapExceeded', `the worker's wallet key locking a ${bondOf} bond above its ${workerCap} per-task cap`);
await call(attackerBot.account, contracts.Pactora, 'cancel', {job_id: jb});

// ── dispute, ruled by the arbiter ────────────────────────────────────────
const j2 = await hire(2);
await call(workerBot.account, contracts.Pactora, 'accept_job', {job_id: j2});
await call(workerBot.account, contracts.Pactora, 'submit_result', {job_id: j2, result_hash: sha(`${run}:result:2`)});
await call(clientBot.account, contracts.Pactora, 'dispute', {job_id: j2, reason_hash: sha('the result is wrong')});
const useDao = slug === 'testnet' || process.argv.includes('--dao');
if (!useDao) {
  const arb = new Account(contracts.Arbiter, provider, new KeyPairSigner(rootKey));
  const ruled = await call(arb, contracts.Pactora, 'resolve_dispute', {job_id: j2, for_worker: false});
  const job2 = await view(contracts.Pactora, 'get_job', {job_id: j2});
  if (job2.state !== 'Refunded' || job2.outcome !== 'DisputeLost') throw new Error(`job ${j2}: ${JSON.stringify(job2)}`);
  ok('dispute ruled for the client by the arbiter: refunded, worker marked DisputeLost', link(ruled.hash));
} else {
  // The arbiter is a 2-of-3 Sputnik DAO: one vote must not rule, two must.
  let daoId;
  let council;
  if (slug === 'sandbox') {
    const members = [await newOwner('council-1'), await newOwner('council-2'), await newOwner('council-3')];
    council = members.map((m) => m.account);
    daoId = `${run}-dao.${rootId}`;
    const wasmPath = process.env.SPUTNIK_DAO_WASM;
    if (!wasmPath || !existsSync(wasmPath)) throw new Error('--dao on the sandbox needs SPUTNIK_DAO_WASM (scripts/create-dao.mjs --fetch-wasm writes it)');
    await deployDaoSandbox(root, daoId, readFileSync(wasmPath), members.map((m) => m.id));
    await call(root, contracts.Pactora, 'set_arbiter', {arbiter: daoId}, {deposit: 1n});
  } else {
    daoId = contracts.Arbiter;
    council = [root, ...councilFromEnv()];
  }
  const roles = await view(contracts.Pactora, 'get_roles');
  if (roles.arbiter !== daoId) throw new Error(`Pactora's arbiter is ${roles.arbiter}, expected the DAO ${daoId}`);
  try {
    // An outsider (the worker's owner, not on the council) cannot even propose.
    await refused(proposeRuling(worker.account, daoId, contracts.Pactora, j2, true, 'pay me'), 'ERR_PERMISSION_DENIED', 'a non-member proposing a ruling to the DAO');
    const p = await proposeRuling(council[0], daoId, contracts.Pactora, j2, false, `Pactora job ${j2}: the result is wrong, refund the client`);
    await vote(council[0], daoId, p.id, p.kind);
    const still = await view(contracts.Pactora, 'get_job', {job_id: j2});
    if (still.state !== 'Disputed') throw new Error(`one council vote ruled job ${j2}: ${JSON.stringify(still)}`);
    ok(`one DAO vote of three does not rule: job ${j2} still Disputed (proposal ${p.id})`);
    const passed2 = await vote(council[1], daoId, p.id, p.kind);
    const job2 = await view(contracts.Pactora, 'get_job', {job_id: j2});
    if (job2.state !== 'Refunded' || job2.outcome !== 'DisputeLost') throw new Error(`job ${j2} after 2 votes: ${JSON.stringify(job2)}`);
    ok(`dispute ruled by the DAO arbiter ${daoId}, 2 of 3 votes: refunded, worker marked DisputeLost`, link(passed2));
  } finally {
    if (slug === 'sandbox') await call(root, contracts.Pactora, 'set_arbiter', {arbiter: contracts.Arbiter}, {deposit: 1n});
  }
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

if (params.review_window <= 60 && params.dispute_grace <= 60) {
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
  const job6 = await view(contracts.Pactora, 'get_job', {job_id: j6});
  await waitUntilChain(job6.dispute_deadline);
  // Inside the grace the arbiter may still rule, and nobody may expire the dispute.
  await refused(
    call(anyone, contracts.Pactora, 'expire_dispute', {job_id: j6}),
    'DeadlineNotPassed',
    `expire_dispute inside the ${params.dispute_grace} s grace after the dispute deadline`,
  );
  const w6 = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: B.id})).balance);
  const c6 = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: A.id})).balance);
  await waitUntilChain(job6.dispute_deadline + params.dispute_grace);
  await call(anyone, contracts.Pactora, 'expire_dispute', {job_id: j6});
  const bond6 = BigInt(job6.dispute_bond);
  const half = bond6 / 2n;
  const wGot = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: B.id})).balance) - w6;
  const cGot = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: A.id})).balance) - c6;
  if (wGot !== 50000n - BigInt(job6.fee) + half || cGot !== bond6 - half) {
    throw new Error(`job ${j6} expired: worker +${wGot}, client +${cGot}, bond ${bond6}, fee ${job6.fee}`);
  }
  ok(`expire_dispute: a dispute nobody rules on pays the worker the job and half the ${bond6} bond (${half}); the other ${bond6 - half} returns to the client`, `job ${j6}`);
} else {
  console.log(`  - auto_approve / expire_dispute skipped: the review window is ${params.review_window} s and the dispute grace ${params.dispute_grace} s here (sandbox uses 10 s)`);
}

// ── a PERSON takes part: no wallet split, no contract change ─────────────
// A person is just an account that is its own owner AND its own wallet, with
// a full-access key. Same contract, same calls; the only difference is who signs.
const person = await newOwner('person');
await call(person.account, contracts.PaymentToken, 'storage_deposit', {account_id: person.id, registration_only: true}, {deposit: BigInt(bounds.min)});
await call(root, contracts.PaymentToken, 'mint', {account_id: person.id, amount: '50000000'});
const pr = await call(person.account, contracts.Pactora, 'register_agent', {wallet: person.id, card_uri: `data:,${run}-person`, per_task_cap: null, daily_cap: null}, {deposit: nearToYocto('0.1')});
const P = pr.value;
const pAccepted = await call(person.account, contracts.Pactora, 'accept_wallet', {agent_id: P});
const agentP = await view(contracts.Pactora, 'get_agent', {agent_id: P});
if (agentP.owner !== person.id || agentP.wallet !== person.id) throw new Error(`person agent: ${JSON.stringify(agentP)}`);
ok(`a person (full-access key) registers agent ${P} as its own wallet and accepts it: owner == wallet == ${person.id}`, link(pAccepted.hash));

await ftCall(person, String(BigInt(params.min_stake) + 100000n), {Stake: {agent_id: P}});
if (!(await view(contracts.Pactora, 'is_hireable', {agent_id: P}))) throw new Error('person not hireable after staking');
ok('the person staked via ft_transfer_call and is hireable');

const pBefore = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: P})).balance);
const jp = (await call(clientBot.account, contracts.Pactora, 'create_job', {client_id: A.id, worker_id: P, amount: '50000', spec_hash: spec('person'), accept_window: 60, work_window: 60})).value;
await call(person.account, contracts.Pactora, 'accept_job', {job_id: jp});
await call(person.account, contracts.Pactora, 'submit_result', {job_id: jp, result_hash: sha(`${run}:result:person`)});
const pApproved = await call(clientBot.account, contracts.Pactora, 'approve', {job_id: jp, review_hash: sha('person did fine')});
const jobP = await view(contracts.Pactora, 'get_job', {job_id: jp});
const pAfter = BigInt((await view(contracts.Pactora, 'purse_of', {agent_id: P})).balance);
const feeP = BigInt(jobP.fee);
if (jobP.state !== 'Settled' || jobP.outcome !== 'Success' || pAfter - pBefore !== 50000n - feeP) {
  throw new Error(`person job ${jp}: ${JSON.stringify(jobP)}; person +${pAfter - pBefore}`);
}
ok(`an agent hires the person: accepted and submitted with its own full-access key, approved — person paid ${50000n - feeP} (fee ${feeP})`, link(pApproved.hash));

// Self-dealing across one owner: the person cannot hire an agent it owns, even
// when that agent's wallet is a different account.
const personBot = await newWallet(person);
const Q = await register(person, personBot);
const agentQ = await view(contracts.Pactora, 'get_agent', {agent_id: Q.id});
if (agentQ.owner !== person.id || agentQ.wallet !== personBot.id) throw new Error(`second person agent: ${JSON.stringify(agentQ)}`);
await ftCall(person, String(BigInt(params.min_stake) + 100000n), {Stake: {agent_id: Q.id}});
await ftCall(person, '1000000', {Fund: {agent_id: P}});
await refused(
  call(person.account, contracts.Pactora, 'create_job', {client_id: P, worker_id: Q.id, amount: '50000', spec_hash: spec('same1'), accept_window: 60, work_window: 60}),
  'SameOwner',
  'the person hiring an agent it owns (wallet is a separate account)',
);
await ftCall(person, '1000000', {Fund: {agent_id: Q.id}});
await refused(
  call(personBot.account, contracts.Pactora, 'create_job', {client_id: Q.id, worker_id: P, amount: '50000', spec_hash: spec('same2'), accept_window: 60, work_window: 60}),
  'SameOwner',
  "the person's second agent hiring the person's first agent (same owner, different wallets)",
);

// ── surplus: a plain ft_transfer credits nobody; only the owner, only the surplus ──
await call(client.account, contracts.PaymentToken, 'ft_transfer', {receiver_id: contracts.Pactora, amount: '777'}, {deposit: 1n});
await refused(
  call(client.account, contracts.Pactora, 'recover_surplus', {amount: '777', receiver: client.id}, {deposit: 1n}),
  'NotOwner',
  'a non-owner recovering the surplus',
);
const clientTokBefore = BigInt(await view(contracts.PaymentToken, 'ft_balance_of', {account_id: client.id}));
const recovered = await call(root, contracts.Pactora, 'recover_surplus', {amount: '1000000', receiver: client.id}, {deposit: 1n, gas: 150n * TGAS});
const clientTokAfter = BigInt(await view(contracts.PaymentToken, 'ft_balance_of', {account_id: client.id}));
if (clientTokAfter - clientTokBefore !== 777n) throw new Error(`recover_surplus moved ${clientTokAfter - clientTokBefore}, expected the 777 surplus`);
ok('recover_surplus: the owner asked for 1000000 and got exactly the 777 a plain ft_transfer had left above the totals', link(recovered.hash));

// ── I1 ───────────────────────────────────────────────────────────────────
const totals = await view(contracts.Pactora, 'get_totals');
const held = await view(contracts.PaymentToken, 'ft_balance_of', {account_id: contracts.Pactora});
if (totals.sum !== held) throw new Error(`I1: get_totals.sum ${totals.sum} != ft_balance_of ${held} (${JSON.stringify(totals)})`);
ok(`I1: the contract holds exactly what it owes — ${held} base units`, JSON.stringify(totals));

console.log(`\nverify-near: ${passed} checks passed on near:${slug}`);
await sweep(ROOT, slug, provider, rootId);
