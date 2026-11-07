#!/usr/bin/env node
/**
 * Live check of scripts/lib/throwaway.mjs on the local sandbox: a test
 * account's key is saved before the account exists, and the sweep deletes the
 * full-access accounts into the deployer, returning their balance.
 *
 *   SANDBOX_KEY_FILE=.sbx/validator_key.json node scripts/sweep-check.mjs
 *
 * The sandbox is named "testnet" to the helpers (keys go to a temp folder),
 * so this exercises the exact testnet code path with no testnet NEAR.
 */
import {existsSync, mkdtempSync, readdirSync, readFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, nearToYocto} from 'near-api-js';
import {keysDir, saveKey, sweep} from './lib/throwaway.mjs';

const k = JSON.parse(readFileSync(process.env.SANDBOX_KEY_FILE || '', 'utf8'));
const provider = new JsonRpcProvider({url: process.env.SANDBOX_RPC || 'http://127.0.0.1:3030'});
if ((await provider.viewNodeStatus()).chain_id === 'testnet' || (await provider.viewNodeStatus()).chain_id === 'mainnet') throw new Error('sweep-check runs on the sandbox only');
const rootId = k.account_id;
const root = new Account(rootId, provider, new KeyPairSigner(KeyPair.fromString(k.secret_key)));
const net = 'testnet'; // what the helpers are told; the node is the sandbox
const dir = mkdtempSync(join(tmpdir(), 'pactora-sweep-'));
const run = `s${Date.now().toString(36)}`;
const balance = (id) => provider.viewAccount({accountId: id, blockQuery: {finality: 'final'}}).then((a) => BigInt(a.amount), () => null);
let failures = 0;
const ok = (m) => console.log(`  ✓ ${m}`);
const bad = (m) => {
  failures++;
  console.log(`  ✗ ${m}`);
};

try {
  console.log(`sweep check on the sandbox (helpers told "${net}", keys in a temp folder)`);
  const owner = KeyPair.fromRandom('ed25519');
  const ownerId = `${run}-owner.${rootId}`;
  saveKey(dir, net, ownerId, owner, {fullAccess: true});
  existsSync(join(keysDir(dir, net), `${ownerId}.json`)) && (await balance(ownerId)) === null
    ? ok('the owner key was saved before its account existed')
    : bad('owner key not saved, or the account already existed');
  await root.signAndSendTransaction({
    receiverId: ownerId,
    actions: [actions.createAccount(), actions.transfer(nearToYocto('2')), actions.addFullAccessKey(owner.getPublicKey())],
    waitUntil: 'FINAL',
  });
  const bot = KeyPair.fromRandom('ed25519');
  const botId = `bot.${ownerId}`;
  saveKey(dir, net, botId, bot, {fullAccess: false});
  await new Account(ownerId, provider, new KeyPairSigner(owner)).signAndSendTransaction({
    receiverId: botId,
    actions: [actions.createAccount(), actions.transfer(nearToYocto('0.1')), actions.addFunctionCallAccessKey(bot.getPublicKey(), rootId, [], nearToYocto('0.05'))],
    waitUntil: 'FINAL',
  });
  const never = `${run}-never.${rootId}`;
  saveKey(dir, net, never, KeyPair.fromRandom('ed25519'), {fullAccess: true});
  ok(`created ${ownerId} (2 NEAR, full access) and ${botId} (function-call key only); ${never} saved, never created`);

  const before = await balance(rootId);
  const ownerBal = await balance(ownerId);
  const lines = [];
  const back = await sweep(dir, net, provider, rootId, (m) => lines.push(m));
  lines.forEach((l) => console.log(l));
  const after = await balance(rootId);
  (await balance(ownerId)) === null ? ok(`${ownerId} was deleted`) : bad(`${ownerId} still exists`);
  back === ownerBal ? ok(`the sweep reports ${back} yocto back, the owner's whole balance`) : bad(`reported ${back}, owner held ${ownerBal}`);
  // The deployer gains the owner's balance; the delete's gas was paid by the deleted account itself.
  after - before > (ownerBal * 99n) / 100n ? ok(`the deployer gained ${(Number((after - before) / 10n ** 18n) / 1e6).toFixed(4)} NEAR`) : bad(`deployer ${before} → ${after}`);
  const left = readdirSync(keysDir(dir, net));
  left.length === 1 && left[0] === `${botId}.json` ? ok('the files of the deleted and never-created accounts were removed; the agent wallet key is kept') : bad(`files left: ${left.join(', ')}`);
  (await balance(botId)) !== null && lines.some((l) => l.includes('agent wallets')) ? ok('the function-call-only wallet is reported, not touched') : bad('the wallet was not reported');
  (await sweep(dir, net, provider, rootId, () => {})) === 0n ? ok('a second sweep returns nothing more') : bad('a second sweep moved funds');
} catch (e) {
  bad(String(e.stack ?? e));
} finally {
  rmSync(dir, {recursive: true, force: true});
}
console.log(failures === 0 ? '\nsweep check passed.' : `\nsweep check FAILED: ${failures}.`);
process.exit(failures === 0 ? 0 : 1);
