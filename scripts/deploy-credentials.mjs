#!/usr/bin/env node
/**
 * Deploy the Pactora Work Credentials contract (contracts/credentials) and
 * add it to deployments/<slug>.json. Standalone: Pactora and mock-usdc are
 * not touched.
 *
 *   node --env-file=.env scripts/deploy-credentials.mjs --network testnet --minter <account>
 *   node scripts/deploy-credentials.mjs --network sandbox --minter <account>
 *   ... --resume   finish a deploy that stopped half-way: an account that
 *       already runs the committed wasm is kept, an account with no code gets
 *       the code and `new`; any other code is refused.
 *
 * The account is credentials.<deployer>. It is created, funded and given the
 * deployer's key in one transaction; then, signed AS THAT ACCOUNT, the wasm is
 * deployed and `new({owner: deployer, minter})` called in a second one (`new`
 * is `#[private]`). The script then reads `owner()` and `minter()` back.
 *
 * testnet: sets `contracts.Credentials` and `wasm.credentials` in
 *   deployments/testnet.json, read-modify-write; every other field is kept.
 * sandbox: deployments/sandbox.json is fixed and committed (scripts/deploy.mjs
 *   checks every contract in it), so it is not written; the result is printed.
 *
 * Env (secrets only in .env, never on the command line):
 *   sandbox  SANDBOX_RPC (default http://127.0.0.1:3030), SANDBOX_KEY_FILE
 *   testnet  DEPLOYER_ACCOUNT_ID, DEPLOYER_SECRET_KEY (ed25519:...), RPC_URL_TESTNET (optional)
 *   both     CREDENTIALS_WASM — optional; default wasm/credentials.wasm,
 *            checked against wasm/SHA256SUMS.
 */
import {readFileSync, writeFileSync, existsSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';
import {codeHash, TGAS} from './lib/dao.mjs';
import {isAccountId, readSums, sha256} from './lib/preflight.mjs';
import {mergeCredentials} from './lib/credentials.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (name) => {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
};
const fail = (msg) => {
  console.error(`deploy-credentials: ${msg}`);
  process.exit(2);
};

const slug = arg('network');
if (slug !== 'sandbox' && slug !== 'testnet') fail('--network must be sandbox or testnet (mainnet is not deployed by this script)');
const minter = arg('minter');
if (!isAccountId(minter)) fail('--minter <account> is required and must be a NEAR account id');
const RESUME = process.argv.includes('--resume');
const chainId = `near:${slug}`;
const net = JSON.parse(readFileSync(join(ROOT, 'config', 'networks.json'), 'utf8'))[chainId];
if (!net) fail(`${chainId} is not in config/networks.json`);

let rpcUrl;
let deployerId;
let secret;
if (slug === 'sandbox') {
  rpcUrl = process.env.SANDBOX_RPC || net.rpcUrls[0];
  const keyFile = process.env.SANDBOX_KEY_FILE;
  if (!keyFile || !existsSync(keyFile)) fail('SANDBOX_KEY_FILE must name the sandbox validator_key.json');
  const k = JSON.parse(readFileSync(keyFile, 'utf8'));
  deployerId = k.account_id;
  secret = k.secret_key;
} else {
  rpcUrl = process.env.RPC_URL_TESTNET || net.rpcUrls[0];
  deployerId = process.env.DEPLOYER_ACCOUNT_ID;
  secret = process.env.DEPLOYER_SECRET_KEY;
  if (!deployerId || !secret) fail('DEPLOYER_ACCOUNT_ID and DEPLOYER_SECRET_KEY must be set (in .env)');
  if (!deployerId.endsWith('.testnet')) fail(`${deployerId} is not a testnet account`);
  if (!minter.endsWith('.testnet')) fail(`--minter ${minter} is not a testnet account`);
}

// The committed build (wasm/credentials.wasm, sha256 in wasm/SHA256SUMS);
// CREDENTIALS_WASM overrides it, e.g. with a CI artifact.
const wasmPath = process.env.CREDENTIALS_WASM || join(ROOT, 'wasm', 'credentials.wasm');
if (!existsSync(wasmPath)) fail(`no wasm at ${wasmPath}`);
const wasm = readFileSync(wasmPath);
const wasmSha = sha256(wasm);
if (!process.env.CREDENTIALS_WASM) {
  const want = readSums(ROOT)['credentials.wasm'];
  if (!want) fail('wasm/SHA256SUMS has no credentials.wasm line');
  if (wasmSha !== want) fail('wasm/credentials.wasm does not match wasm/SHA256SUMS');
}

const provider = new JsonRpcProvider({url: rpcUrl});
const status = await provider.viewNodeStatus();
if (status.chain_id !== net.nearNetworkId) fail(`RPC ${rpcUrl} is "${status.chain_id}", expected "${net.nearNetworkId}"`);

const keyPair = KeyPair.fromString(secret);
const deployer = new Account(deployerId, provider, new KeyPairSigner(keyPair));
/** The code hash of an account with no contract. */
const NO_CODE = '11111111111111111111111111111111';
const accountId = `credentials.${deployerId}`;
const init = {owner: deployerId, minter};
// Contract storage is 1 NEAR per 100 kB; one more for state and the first tokens.
const fundNear = String(Math.ceil(wasm.length / 100_000) + 1);

const view = await provider.viewAccount({accountId, blockQuery: {finality: 'final'}}).catch(() => null);
console.log(`deploying ${accountId} to ${chainId} from ${deployerId}${RESUME ? ' (resume)' : ''}; minter ${minter}`);
let deploy = true;
if (view) {
  if (!RESUME) fail(`${accountId} already exists — this script does not overwrite a deployment (after a run that stopped half-way, re-run with --resume)`);
  const onChain = view.code_hash ?? view.codeHash;
  if (typeof onChain !== 'string') fail(`${accountId}: no code hash in view_account — refusing to resume`);
  if (onChain === codeHash(wasm)) {
    console.log(`  ${accountId}  already runs the committed wasm (resume: kept)`);
    deploy = false;
  } else if (onChain !== NO_CODE) {
    fail(`${accountId} runs other code (${onChain}) — refusing to resume over it`);
  } else {
    console.log(`  ${accountId}  exists with no code (resume: deploying)`);
  }
} else {
  const out = await deployer.signAndSendTransaction({
    receiverId: accountId,
    actions: [actions.createAccount(), actions.transfer(nearToYocto(fundNear)), actions.addFullAccessKey(keyPair.getPublicKey())],
    waitUntil: 'FINAL',
  });
  console.log(`  ${accountId}  created with ${fundNear} NEAR  ${out.transaction.hash}`);
}
if (deploy) {
  const self = new Account(accountId, provider, new KeyPairSigner(keyPair));
  const d = await self.signAndSendTransaction({
    receiverId: accountId,
    actions: [actions.deployContract(wasm), actions.functionCall('new', init, 300n * TGAS, 0n)],
    waitUntil: 'FINAL',
  });
  console.log(`  ${accountId}  deployed + new  ${d.transaction.hash}`);
}

// What was deployed must be what was asked for, or nothing is written.
const owner = await provider.callFunction({contractId: accountId, method: 'owner', args: {}});
const onChainMinter = await provider.callFunction({contractId: accountId, method: 'minter', args: {}});
if (owner !== deployerId || onChainMinter !== minter) {
  fail(`${accountId} has owner ${owner}, minter ${onChainMinter}; expected ${deployerId}, ${minter} — deployments not written`);
}
const meta = await provider.callFunction({contractId: accountId, method: 'nft_metadata', args: {}});
if (meta.symbol !== 'PWORK') fail(`nft_metadata symbol is ${meta.symbol}, expected PWORK`);

if (slug === 'testnet') {
  const file = join(ROOT, 'deployments', `${slug}.json`);
  const merged = mergeCredentials(JSON.parse(readFileSync(file, 'utf8')), {accountId, wasmSha256: wasmSha});
  writeFileSync(file, `${JSON.stringify(merged, null, 2)}\n`);
  console.log(`updated deployments/${slug}.json (contracts.Credentials, wasm.credentials)`);
} else {
  console.log(`sandbox: deployments/sandbox.json is fixed and not written; Credentials = ${accountId}`);
}
console.log(`credentials wasm sha256 ${wasmSha}`);
