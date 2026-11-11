#!/usr/bin/env node
/**
 * Deploy mock-usdc and pactora, and write deployments/<slug>.json.
 *
 *   node scripts/deploy.mjs --network sandbox   # local near-sandbox (Docker)
 *   node scripts/deploy.mjs --network testnet   # NEAR testnet — never mainnet
 *   node scripts/deploy.mjs --network testnet --resume
 *       finish a deploy that stopped half-way (e.g. the RPC dropped the
 *       connection): an account that already runs the committed wasm is kept,
 *       an account with no code gets the code and `new`; any other code, or
 *       an account the deployer's key cannot sign for, is refused.
 *
 * Each contract account is created, funded and given the deployer's key in
 * one transaction; then, signed AS THAT ACCOUNT with the same key, its wasm
 * is deployed and `new` called in a second one. `new` is `#[private]` on both
 * contracts (only the contract account may call it), so nobody can call it
 * on freshly deployed code first. The params are config/params.<slug>.json
 * `onchain`, byte for byte, and the script reads `get_params` back and
 * refuses to write the deployment if they differ.
 *
 * Env (secrets only in .env, never on the command line):
 *   sandbox  SANDBOX_RPC (default http://127.0.0.1:3030),
 *            SANDBOX_KEY_FILE — the sandbox's validator_key.json (test.near)
 *   testnet  DEPLOYER_ACCOUNT_ID, DEPLOYER_SECRET_KEY (ed25519:...),
 *            ARBITER_ACCOUNT_ID — the 2-of-3 Sputnik DAO (scripts/create-dao.mjs)
 *   both     PACTORA_WASM, MOCK_USDC_WASM — optional; default wasm/*.wasm,
 *            checked against wasm/SHA256SUMS. Its sha256 is recorded on testnet.
 */
import {createHash} from 'node:crypto';
import {readFileSync, writeFileSync, existsSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';
import {codeHash} from './lib/dao.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (name) => {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
};
const fail = (msg) => {
  console.error(`deploy: ${msg}`);
  process.exit(2);
};

const slug = arg('network');
if (slug !== 'sandbox' && slug !== 'testnet') fail('--network must be sandbox or testnet (mainnet is not deployed by this script)');
const chainId = `near:${slug}`;
const networks = JSON.parse(readFileSync(join(ROOT, 'config', 'networks.json'), 'utf8'));
const net = networks[chainId];
if (!net) fail(`${chainId} is not in config/networks.json`);
const params = JSON.parse(readFileSync(join(ROOT, 'config', `params.${slug}.json`), 'utf8')).onchain;

let rpcUrl;
let deployerId;
let secret;
let arbiter;
if (slug === 'sandbox') {
  rpcUrl = process.env.SANDBOX_RPC || net.rpcUrls[0];
  const keyFile = process.env.SANDBOX_KEY_FILE;
  if (!keyFile || !existsSync(keyFile)) fail('SANDBOX_KEY_FILE must name the sandbox validator_key.json');
  const k = JSON.parse(readFileSync(keyFile, 'utf8'));
  deployerId = k.account_id;
  secret = k.secret_key;
  if (deployerId !== 'test.near') fail(`the sandbox root is ${deployerId}, deployments/sandbox.json expects test.near`);
} else {
  rpcUrl = process.env.RPC_URL_TESTNET || net.rpcUrls[0];
  deployerId = process.env.DEPLOYER_ACCOUNT_ID;
  secret = process.env.DEPLOYER_SECRET_KEY;
  arbiter = process.env.ARBITER_ACCOUNT_ID;
  if (!deployerId || !secret) fail('DEPLOYER_ACCOUNT_ID and DEPLOYER_SECRET_KEY must be set (in .env)');
  if (!deployerId.endsWith('.testnet')) fail(`${deployerId} is not a testnet account`);
  if (!arbiter) fail('ARBITER_ACCOUNT_ID must name the arbiter DAO — create it with scripts/create-dao.mjs');
}

// Default: the committed build in wasm/ (built in the pinned cargo-near
// image, sha256 in wasm/SHA256SUMS), so a judge needs no Rust toolchain.
// PACTORA_WASM / MOCK_USDC_WASM override it, e.g. with a fresh CI artifact.
const sha = (b) => createHash('sha256').update(b).digest('hex');
const sums = Object.fromEntries(
  readFileSync(join(ROOT, 'wasm', 'SHA256SUMS'), 'utf8')
    .trim()
    .split('\n')
    .map((l) => l.trim())
    .map((l) => l.split(/\s+\*?/).reverse()),
);
const loadWasm = (envName, file) => {
  // `||`, not `??`: a line left empty in .env (as copied from .env.example) is "", not unset.
  const p = process.env[envName] || join(ROOT, 'wasm', file);
  if (!existsSync(p)) fail(`${envName}: no wasm at ${p}`);
  const bytes = readFileSync(p);
  if (!process.env[envName] && sha(bytes) !== sums[file]) fail(`wasm/${file} does not match wasm/SHA256SUMS`);
  return bytes;
};
const pactoraWasm = loadWasm('PACTORA_WASM', 'pactora.wasm');
const usdcWasm = loadWasm('MOCK_USDC_WASM', 'mock_usdc.wasm');

const provider = new JsonRpcProvider({url: rpcUrl});
const status = await provider.viewNodeStatus();
if (status.chain_id !== net.nearNetworkId) fail(`RPC ${rpcUrl} is "${status.chain_id}", expected "${net.nearNetworkId}"`);

const keyPair = KeyPair.fromString(secret);
const deployer = new Account(deployerId, provider, new KeyPairSigner(keyPair));
const TGAS = 1_000_000_000_000n;

const RESUME = process.argv.includes('--resume');
/** The code hash of an account with no contract. */
const NO_CODE = '11111111111111111111111111111111';

const exists = async (id) =>
  provider.viewAccount({accountId: id, blockQuery: {finality: 'final'}}).then(
    () => true,
    () => false,
  );

/**
 * Create the account, fund it and add the deployer's key; then, as that
 * account, deploy the code and call `new` (private: only the account itself
 * may initialise, so the two steps need no atomicity).
 */
async function createAndDeploy(accountId, wasm, nearAmount, init) {
  let out = null;
  if (await exists(accountId)) {
    if (!RESUME) {
      fail(
        `${accountId} already exists — this script does not overwrite a deployment ` +
          '(after a run that stopped half-way, re-run with --resume)',
      );
    }
    const view = await provider.viewAccount({accountId, blockQuery: {finality: 'final'}});
    // near-api-js returns the RPC's own field name here, whatever its types say.
    const onChain = view.code_hash ?? view.codeHash;
    if (typeof onChain !== 'string') fail(`${accountId}: no code hash in view_account — refusing to resume`);
    if (wasm && onChain === codeHash(wasm)) {
      console.log(`  ${accountId}  already runs the committed wasm (resume: kept)`);
      return null;
    }
    if (onChain !== NO_CODE) fail(`${accountId} runs other code (${onChain}) — refusing to resume over it`);
    if (!wasm) return null;
    console.log(`  ${accountId}  exists with no code (resume: deploying)`);
  } else {
    out = await deployer.signAndSendTransaction({
      receiverId: accountId,
      actions: [actions.createAccount(), actions.transfer(nearToYocto(nearAmount)), actions.addFullAccessKey(keyPair.getPublicKey())],
      waitUntil: 'FINAL',
    });
    console.log(`  ${accountId}  created  ${out.transaction.hash}`);
  }
  if (wasm) {
    const self = new Account(accountId, provider, new KeyPairSigner(keyPair));
    const d = await self.signAndSendTransaction({
      receiverId: accountId,
      actions: [actions.deployContract(wasm), actions.functionCall('new', init, 300n * TGAS, 0n)],
      waitUntil: 'FINAL',
    });
    console.log(`  ${accountId}  deployed + new  ${d.transaction.hash}`);
  }
  return out;
}

const parent = deployerId;
const names = {
  Pactora: `pactora.${parent}`,
  PaymentToken: `musdc.${parent}`,
  Arbiter: arbiter ?? `arbiter.${parent}`,
};
console.log(`deploying to ${chainId} from ${deployerId}${RESUME ? ' (resume)' : ''}`);
const resumeHead = RESUME ? (await provider.viewBlock({finality: 'final'})).header.height : null;

const usdcOut = await createAndDeploy(names.PaymentToken, usdcWasm, slug === 'sandbox' ? '10' : '3', {owner: deployerId});
if (slug === 'sandbox') await createAndDeploy(names.Arbiter, null, '10');
await createAndDeploy(names.Pactora, pactoraWasm, slug === 'sandbox' ? '20' : '6', {
  owner: deployerId,
  token: names.PaymentToken,
  arbiter: names.Arbiter,
  pauser: deployerId,
  fee_recipient: deployerId,
  params,
});

// The contract must hold a token balance to receive ft_transfer_call (NEP-145).
const bounds = await provider.callFunction({contractId: names.PaymentToken, method: 'storage_balance_bounds', args: {}});
await deployer.callFunction({
  contractId: names.PaymentToken,
  methodName: 'storage_deposit',
  args: {account_id: names.Pactora, registration_only: true},
  deposit: BigInt(bounds.min),
  gas: 30n * TGAS,
});

// What was deployed must be what the file says, or nothing is written.
const onChain = await provider.callFunction({contractId: names.Pactora, method: 'get_params', args: {}});
if (JSON.stringify(onChain) !== JSON.stringify(params)) {
  fail(`get_params differs from params.${slug}.json onchain:\n${JSON.stringify(onChain)}\n${JSON.stringify(params)}`);
}
const roles = await provider.callFunction({contractId: names.Pactora, method: 'get_roles', args: {}});
if (roles.token !== names.PaymentToken || roles.arbiter !== names.Arbiter) fail(`get_roles: ${JSON.stringify(roles)}`);

// The indexer starts here. A resumed run has no creation receipt for the
// token, so it starts at the head as the resume began: before Pactora's
// `new`, the first block anything the indexer reads can appear in.
const startBlock = usdcOut
  ? (await provider.viewBlock({blockId: usdcOut.transaction_outcome.block_hash})).header.height
  : resumeHead;
const deployment = {
  chainId,
  deployedAt: slug === 'sandbox' ? 'fixed' : new Date().toISOString(),
  startBlock: slug === 'sandbox' ? 0 : startBlock,
  ...(slug === 'testnet' && process.env.DEPLOY_COMMIT ? {commit: process.env.DEPLOY_COMMIT} : {}),
  contracts: names,
  ...(slug === 'testnet' ? {wasm: {pactora: sha(pactoraWasm), mockUsdc: sha(usdcWasm)}} : {}),
};

const file = join(ROOT, 'deployments', `${slug}.json`);
if (slug === 'sandbox') {
  // The sandbox file is fixed and committed; check this run produced exactly it.
  const want = JSON.parse(readFileSync(file, 'utf8'));
  for (const [k, v] of Object.entries(want.contracts)) {
    if (names[k] !== v) fail(`sandbox deployed ${k}=${names[k]}, deployments/sandbox.json says ${v}`);
  }
  console.log('sandbox deployment matches deployments/sandbox.json');
} else {
  writeFileSync(file, `${JSON.stringify(deployment, null, 2)}\n`);
  console.log(`wrote deployments/${slug}.json`);
}
console.log(`pactora wasm sha256 ${sha(pactoraWasm)}\nmock-usdc wasm sha256 ${sha(usdcWasm)}`);
