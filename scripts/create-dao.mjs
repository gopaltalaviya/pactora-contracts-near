#!/usr/bin/env node
/**
 * The arbiter DAO: a 2-of-3 Sputnik DAO council that rules Pactora disputes.
 *
 *   node scripts/create-dao.mjs --fetch-wasm <file>   # the DAO code, for the sandbox (no factory there)
 *   node scripts/create-dao.mjs --network testnet     # create it on testnet, before scripts/deploy.mjs
 *
 * --fetch-wasm reads the code of a DAO the testnet factory deployed and writes
 * it only if its hash equals the factory's `get_default_code_hash` — the
 * sandbox then runs exactly the code testnet runs (verify-near --dao).
 *
 * --network testnet (needs DEPLOYER_ACCOUNT_ID / DEPLOYER_SECRET_KEY in .env):
 *   1. creates council-1.<deployer> and council-2.<deployer>, each with a new
 *      full-access key, and APPENDS their ids and keys to .env (never printed);
 *   2. asks sputnikv2.testnet to create `pactora-arbiter-<deployer name>` with
 *      the council {deployer, council-1, council-2}, threshold 2 of 3;
 *   3. appends ARBITER_ACCOUNT_ID to .env and checks the policy on chain.
 * Re-running is safe: whatever already exists (in .env and on chain) is kept.
 */
import {appendFileSync, existsSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';
import {DAO_CODE_HASH, FACTORY_TESTNET, codeHash, createDaoTestnet} from './lib/dao.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const ENV_FILE = join(ROOT, '.env');
const TESTNET_RPC = process.env.RPC_URL_TESTNET ?? 'https://rpc.testnet.near.org';
const arg = (n) => (process.argv.includes(`--${n}`) ? process.argv[process.argv.indexOf(`--${n}`) + 1] : undefined);
const fail = (m) => {
  console.error(`create-dao: ${m}`);
  process.exit(2);
};
const provider = new JsonRpcProvider({url: TESTNET_RPC});
const status = await provider.viewNodeStatus();
if (status.chain_id !== 'testnet') fail(`RPC ${TESTNET_RPC} is "${status.chain_id}", expected "testnet"`);
const view = (contractId, method, args = {}) => provider.callFunction({contractId, method, args, blockQuery: {finality: 'final'}});
const exists = (id) => provider.viewAccount({accountId: id, blockQuery: {finality: 'final'}}).then(() => true, () => false);

const factoryHash = await view(FACTORY_TESTNET, 'get_default_code_hash');
if (factoryHash !== DAO_CODE_HASH) fail(`the factory's default DAO code is now ${factoryHash}, lib/dao.mjs pins ${DAO_CODE_HASH} — review the new version first`);

const fetchTo = arg('fetch-wasm');
if (fetchTo) {
  const n = Number(await view(FACTORY_TESTNET, 'get_number_daos'));
  const daos = await view(FACTORY_TESTNET, 'get_daos', {from_index: Math.max(0, n - 10), limit: 10});
  for (const dao of daos.reverse()) {
    const code = await provider.query({request_type: 'view_code', account_id: dao, finality: 'final'});
    const wasm = Buffer.from(code.code_base64, 'base64');
    if (codeHash(wasm) !== DAO_CODE_HASH) continue;
    writeFileSync(fetchTo, wasm);
    console.log(`wrote ${wasm.length} bytes from ${dao}; hash ${DAO_CODE_HASH} = the factory's default code`);
    process.exit(0);
  }
  fail('none of the last 10 DAOs runs the default code');
}

if (arg('network') !== 'testnet') fail('--network testnet, or --fetch-wasm <file>');
const deployerId = process.env.DEPLOYER_ACCOUNT_ID;
const secret = process.env.DEPLOYER_SECRET_KEY;
if (!deployerId?.endsWith('.testnet') || !secret) fail('DEPLOYER_ACCOUNT_ID (.testnet) and DEPLOYER_SECRET_KEY must be set in .env');
const deployer = new Account(deployerId, provider, new KeyPairSigner(KeyPair.fromString(secret)));

const remember = (name, value) => {
  appendFileSync(ENV_FILE, `${name}=${value}\n`);
  process.env[name] = value;
};
if (existsSync(ENV_FILE) && !readFileSync(ENV_FILE, 'utf8').endsWith('\n')) appendFileSync(ENV_FILE, '\n');

const council = [deployerId];
for (const i of [1, 2]) {
  const id = process.env[`COUNCIL_${i}_ACCOUNT_ID`] ?? `council-${i}.${deployerId}`;
  if (!process.env[`COUNCIL_${i}_SECRET_KEY`]) {
    if (await exists(id)) fail(`${id} exists but its key is not in .env (COUNCIL_${i}_SECRET_KEY)`);
    const kp = KeyPair.fromRandom('ed25519');
    await deployer.signAndSendTransaction({
      receiverId: id,
      actions: [actions.createAccount(), actions.transfer(nearToYocto('1')), actions.addFullAccessKey(kp.getPublicKey())],
      waitUntil: 'FINAL',
    });
    remember(`COUNCIL_${i}_ACCOUNT_ID`, id);
    remember(`COUNCIL_${i}_SECRET_KEY`, kp.toString());
    console.log(`  created ${id} (key saved to .env)`);
  } else {
    console.log(`  ${id} (from .env)`);
  }
  council.push(id);
}

const name = `pactora-arbiter-${deployerId.replace(/\.testnet$/, '').replace(/[^a-z0-9-]/g, '-')}`.slice(0, 40);
let daoId = `${name}.${FACTORY_TESTNET}`;
if (await exists(daoId)) {
  console.log(`  ${daoId} exists`);
} else {
  const out = await createDaoTestnet(deployer, name, council);
  daoId = out.daoId;
  console.log(`  created ${daoId}  https://testnet.nearblocks.io/txns/${out.hash}`);
}

// The DAO on chain must be exactly the council and threshold asked for.
const policy = await view(daoId, 'get_policy');
const members = policy.roles.find((r) => r.name === 'council')?.kind?.Group ?? [];
if (JSON.stringify([...members].sort()) !== JSON.stringify([...council].sort())) fail(`${daoId} council is ${JSON.stringify(members)}, expected ${JSON.stringify(council)}`);
if (JSON.stringify(policy.default_vote_policy.threshold) !== '[1,2]') fail(`${daoId} threshold is ${JSON.stringify(policy.default_vote_policy.threshold)}`);
if (process.env.ARBITER_ACCOUNT_ID !== daoId) remember('ARBITER_ACCOUNT_ID', daoId);
console.log(`arbiter DAO ${daoId}: council ${council.join(', ')}, 2 of 3. ARBITER_ACCOUNT_ID is in .env; next: scripts/deploy.mjs --network testnet`);
