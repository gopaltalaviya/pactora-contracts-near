/**
 * Unit tests for the testnet preflight (scripts/lib/preflight.mjs), against a
 * fake RPC and a temp copy of config/ and wasm/ (the committed wasm is never
 * touched). Run: npm run test:preflight
 *
 * The fake proves only that each check refuses what it should; the real-node
 * check is `--network sandbox` (README "Testnet").
 */
import assert from 'node:assert/strict';
import {copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {afterEach, beforeEach, test} from 'node:test';
import {fileURLToPath} from 'node:url';
import {KeyPair} from 'near-api-js';
import {codeHash} from './lib/dao.mjs';
import {preflight} from './lib/preflight.mjs';

const REPO = join(dirname(fileURLToPath(import.meta.url)), '..');
const NEAR = 10n ** 24n;
const DEPLOYER = 'owner.testnet';
const key = KeyPair.fromRandom('ed25519');
const SECRET = key.toString();
const PK = key.getPublicKey().toString();
const pactoraHash = codeHash(readFileSync(join(REPO, 'wasm', 'pactora.wasm')));
const params = JSON.parse(readFileSync(join(REPO, 'config', 'params.testnet.json'), 'utf8')).onchain;

let root;
beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), 'preflight-'));
  for (const d of ['config', 'wasm', 'deployments']) mkdirSync(join(root, d));
  for (const f of ['networks.json', 'params.testnet.json', 'params.sandbox.json']) copyFileSync(join(REPO, 'config', f), join(root, 'config', f));
  for (const f of ['pactora.wasm', 'mock_usdc.wasm', 'SHA256SUMS']) copyFileSync(join(REPO, 'wasm', f), join(root, 'wasm', f));
});
afterEach(() => rmSync(root, {recursive: true, force: true}));

/** A fake RPC: accounts {id: {amount, storage_usage, code_hash, keys: {pk: permission}}}. */
function fakeRpc({chainId = 'testnet', accounts = {}, views = {}} = {}) {
  return {
    chainId: async () => chainId,
    account: async (id) => accounts[id] ?? null,
    accessKey: async (id, pk) => accounts[id]?.keys?.[pk] ?? null,
    view: async (contractId, method) => {
      const v = views[`${contractId}.${method}`];
      if (v === undefined) throw new Error(`no view ${contractId}.${method}`);
      return v;
    },
  };
}
const funded = (near, keys = {[PK]: 'FullAccess'}) => ({amount: String(BigInt(near) * NEAR), storage_usage: 182, code_hash: '11111111111111111111111111111111', keys});
const goodEnv = () => ({DEPLOYER_ACCOUNT_ID: DEPLOYER, DEPLOYER_SECRET_KEY: SECRET});
const goodRpc = (over = {}) => fakeRpc({accounts: {[DEPLOYER]: funded(30)}, ...over});

const run = (env, rpc, slug = 'testnet') => preflight({slug, env, root, rpc});
const failed = (r) => r.checks.filter((c) => c.status !== 'ok');
/** NO-GO, with `name` failing and its message matching `re`. */
function refused(r, name, re) {
  assert.equal(r.go, false, 'expected NO-GO');
  const c = r.checks.find((x) => x.name === name);
  assert.ok(c, `no check named ${name}: ${r.checks.map((x) => x.name)}`);
  assert.equal(c.status, 'FAIL', `${name} should FAIL: ${c.detail}`);
  assert.match(c.detail, re);
  for (const x of r.checks) assert.ok(!x.detail.includes(SECRET.slice(8)), `check ${x.name} printed the secret key`);
}

test('a good setup is GO, with the full README command list', async () => {
  const r = await run(goodEnv(), goodRpc());
  assert.deepEqual(failed(r), []);
  assert.equal(r.go, true);
  assert.match(r.next.join('\n'), /create-dao\.mjs --network testnet[\s\S]*deploy\.mjs --network testnet[\s\S]*verify-near\.mjs --network testnet/);
  for (const c of r.checks) assert.ok(!c.detail.includes(SECRET.slice(8)), 'secret printed');
});

test('wrong network: anything but testnet (or sandbox) is refused', async () => {
  refused(await run(goodEnv(), goodRpc(), 'mainnet'), 'network', /mainnet.*refused|testnet/);
});

test('wrong network: an RPC that answers mainnet is refused', async () => {
  refused(await run(goodEnv(), goodRpc({chainId: 'mainnet'})), 'network', /"mainnet", expected "testnet"/);
});

test('missing key is refused', async () => {
  const env = goodEnv();
  delete env.DEPLOYER_SECRET_KEY;
  refused(await run(env, goodRpc()), 'deployer key', /DEPLOYER_SECRET_KEY is not set/);
});

test('missing account id is refused', async () => {
  const env = goodEnv();
  env.DEPLOYER_ACCOUNT_ID = '';
  refused(await run(env, goodRpc()), 'deployer id', /DEPLOYER_ACCOUNT_ID is not set/);
});

test('a non-testnet account id is refused', async () => {
  refused(await run({...goodEnv(), DEPLOYER_ACCOUNT_ID: 'owner.near'}, goodRpc()), 'deployer id', /not a \.testnet account/);
});

test('a malformed account id is refused', async () => {
  refused(await run({...goodEnv(), DEPLOYER_ACCOUNT_ID: 'Owner..testnet'}, goodRpc()), 'deployer id', /not a valid NEAR account id/);
});

test('malformed key is refused, and not printed', async () => {
  refused(await run({...goodEnv(), DEPLOYER_SECRET_KEY: SECRET.slice(8)}, goodRpc()), 'deployer key', /ed25519:/);
  refused(await run({...goodEnv(), DEPLOYER_SECRET_KEY: `${SECRET.slice(0, -6)}0OIl00`}, goodRpc()), 'deployer key', /not a valid ed25519 secret key/);
});

test('an account that does not exist is refused', async () => {
  refused(await run(goodEnv(), fakeRpc()), 'account', /does not exist on testnet/);
});

test('a key that is not on the account is refused', async () => {
  const other = KeyPair.fromRandom('ed25519').getPublicKey().toString();
  refused(await run(goodEnv(), goodRpc({accounts: {[DEPLOYER]: funded(30, {[other]: 'FullAccess'})}})), 'access key', /is not a key of owner\.testnet/);
});

test('a function-call key instead of full access is refused', async () => {
  const fc = {FunctionCall: {allowance: null, receiver_id: 'x.testnet', method_names: []}};
  refused(await run(goodEnv(), goodRpc({accounts: {[DEPLOYER]: funded(30, {[PK]: fc})}})), 'access key', /function-call key.*full-access/);
});

test('low balance is refused, with the shortfall', async () => {
  refused(await run(goodEnv(), goodRpc({accounts: {[DEPLOYER]: funded(10)}})), 'balance', /short by 15\.\d+ NEAR/);
});

test('a wasm hash mismatch (temp copy) is refused', async () => {
  const p = join(root, 'wasm', 'pactora.wasm');
  const b = readFileSync(p);
  b[b.length - 1] ^= 0xff;
  writeFileSync(p, b);
  refused(await run(goodEnv(), goodRpc()), 'wasm', /pactora\.wasm does not match wasm\/SHA256SUMS/);
});

test('a params file that does not parse is refused', async () => {
  writeFileSync(join(root, 'config', 'params.testnet.json'), '{"onchain": {');
  refused(await run(goodEnv(), goodRpc()), 'params', /does not parse/);
});

test('a params file without dispute_grace is refused', async () => {
  const p = JSON.parse(readFileSync(join(root, 'config', 'params.testnet.json'), 'utf8'));
  delete p.onchain.dispute_grace;
  writeFileSync(join(root, 'config', 'params.testnet.json'), JSON.stringify(p));
  refused(await run(goodEnv(), goodRpc()), 'params', /no onchain\.dispute_grace/);
});

test('deploy would collide: no deployments file but pactora.<deployer> exists', async () => {
  const rpc = goodRpc({accounts: {[DEPLOYER]: funded(30), [`pactora.${DEPLOYER}`]: funded(6, {})}});
  refused(await run(goodEnv(), rpc), 'deployment', /pactora\.owner\.testnet already exists/);
});

const ARBITER = 'pactora-arbiter-owner.sputnikv2.testnet';
function deployed(over = {}) {
  const sums = Object.fromEntries(readFileSync(join(root, 'wasm', 'SHA256SUMS'), 'utf8').trim().split('\n').map((l) => l.trim().split(/\s+\*?/).reverse()));
  const d = {
    chainId: 'near:testnet',
    deployedAt: '2026-11-10T00:00:00.000Z',
    startBlock: 1,
    contracts: {Pactora: `pactora.${DEPLOYER}`, PaymentToken: `musdc.${DEPLOYER}`, Arbiter: ARBITER},
    wasm: {pactora: sums['pactora.wasm'], mockUsdc: sums['mock_usdc.wasm']},
    ...over,
  };
  writeFileSync(join(root, 'deployments', 'testnet.json'), JSON.stringify(d));
}
const deployedRpc = (pactora = {}) =>
  fakeRpc({
    accounts: {[DEPLOYER]: funded(10), [`pactora.${DEPLOYER}`]: {...funded(6, {}), code_hash: pactoraHash, ...pactora}, [ARBITER]: funded(6, {})},
    views: {
      [`pactora.${DEPLOYER}.get_roles`]: {token: `musdc.${DEPLOYER}`, arbiter: ARBITER},
      [`pactora.${DEPLOYER}.get_params`]: params,
    },
  });

test('re-run: a consistent deployments/testnet.json is GO, and the budget is only verify-near', async () => {
  deployed();
  const r = await run({...goodEnv(), ARBITER_ACCOUNT_ID: ARBITER}, deployedRpc());
  assert.deepEqual(failed(r), []);
  assert.equal(r.go, true);
  assert.doesNotMatch(r.next.join('\n'), /deploy\.mjs|create-dao\.mjs/);
});

test('re-run: a deployment of different wasm is refused', async () => {
  deployed({wasm: {pactora: '00'.repeat(32), mockUsdc: '00'.repeat(32)}});
  refused(await run({...goodEnv(), ARBITER_ACCOUNT_ID: ARBITER}, deployedRpc()), 'deployment', /wasm\.pactora .* not the committed build in wasm\/SHA256SUMS/);
});

test('re-run: a deployment whose arbiter is not ARBITER_ACCOUNT_ID is refused', async () => {
  deployed();
  refused(await run({...goodEnv(), ARBITER_ACCOUNT_ID: 'other-dao.sputnikv2.testnet'}, deployedRpc()), 'deployment', /Arbiter .* ARBITER_ACCOUNT_ID/);
});

test('re-run: on-chain code that is not the committed wasm is refused', async () => {
  deployed();
  refused(await run({...goodEnv(), ARBITER_ACCOUNT_ID: ARBITER}, deployedRpc({code_hash: '11111111111111111111111111111111'})), 'deployment', /code .* not wasm\/pactora\.wasm/);
});

test('re-run: a deployments file for another network is refused', async () => {
  deployed({chainId: 'near:sandbox'});
  refused(await run({...goodEnv(), ARBITER_ACCOUNT_ID: ARBITER}, deployedRpc()), 'deployment', /chainId "near:sandbox"/);
});

test('malformed COUNCIL_* and ARBITER values are refused', async () => {
  refused(await run({...goodEnv(), COUNCIL_1_ACCOUNT_ID: 'Bad Id'}, goodRpc()), 'council/arbiter', /COUNCIL_1_ACCOUNT_ID/);
  refused(await run({...goodEnv(), COUNCIL_2_SECRET_KEY: 'nope'}, goodRpc()), 'council/arbiter', /COUNCIL_2_SECRET_KEY/);
  refused(await run({...goodEnv(), ARBITER_ACCOUNT_ID: 'arbiter.testnet'}, goodRpc()), 'council/arbiter', /ARBITER_ACCOUNT_ID .*sputnikv2\.testnet/);
  refused(await run({...goodEnv(), COUNCIL_1_ACCOUNT_ID: DEPLOYER}, goodRpc()), 'council/arbiter', /distinct/);
});

test('a council account that exists without its key in .env is refused (create-dao would stop)', async () => {
  const rpc = goodRpc({accounts: {[DEPLOYER]: funded(30), 'mine.testnet': funded(1, {})}});
  refused(await run({...goodEnv(), COUNCIL_1_ACCOUNT_ID: 'mine.testnet'}, rpc), 'council/arbiter', /mine\.testnet exists but COUNCIL_1_SECRET_KEY is not set/);
});

test('well-formed council values are GO', async () => {
  const c1 = KeyPair.fromRandom('ed25519');
  const rpc = goodRpc({accounts: {[DEPLOYER]: funded(30), 'mine.testnet': funded(1, {[c1.getPublicKey().toString()]: 'FullAccess'})}});
  const r = await run({...goodEnv(), COUNCIL_1_ACCOUNT_ID: 'mine.testnet', COUNCIL_1_SECRET_KEY: c1.toString()}, rpc);
  assert.deepEqual(failed(r), []);
  for (const c of r.checks) assert.ok(!c.detail.includes(c1.toString().slice(8)), 'council secret printed');
});
