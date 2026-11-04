#!/usr/bin/env node
/**
 * Add contracts deployed by an add-on script (DeployValidator.s.sol,
 * DeployAccounts.s.sol) to the live deployment record, deployments/<chainId>.json.
 *
 * Generated, like every address here: read from the broadcast artifact, then
 * checked against the chain before anything is written —
 *   - SettlementValidator: bound to the record's TaskEscrow and the
 *     configured Validation Registry;
 *   - AgentAccountFactory: bound to the record's PaymentToken, and its
 *     implementation's runtime code is byte-for-byte the compiled AgentAccount;
 *   - AgentDelegate: its runtime code is byte-for-byte the compiled contract.
 * Neither AgentAccount nor AgentDelegate has immutables, so equal code is the
 * exact test. One failed check and nothing is written.
 *
 *   node scripts/record-addon.mjs DeployValidator.s.sol 10143
 *   node scripts/record-addon.mjs DeployAccounts.s.sol 10143
 */

import {readFileSync, writeFileSync, existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const [script, chainArg] = process.argv.slice(2);
const chainId = Number(chainArg);
if (!script || !Number.isInteger(chainId)) {
  console.error('usage: record-addon.mjs <Script.s.sol> <chainId>');
  process.exit(1);
}

const net = JSON.parse(readFileSync(join(ROOT, 'config/networks.json'), 'utf8'))[String(chainId)];
const target = join(ROOT, 'deployments', `${chainId}.json`);
const runFile = join(ROOT, 'broadcast', script, String(chainId), 'run-latest.json');
if (!net || !existsSync(target) || !existsSync(runFile)) {
  console.error(`need config for ${chainId}, ${target} and ${runFile}`);
  process.exit(1);
}
const record = JSON.parse(readFileSync(target, 'utf8'));
const run = JSON.parse(readFileSync(runFile, 'utf8'));

async function rpc(method, params) {
  const res = await fetch(net.rpcUrls[0], {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method, params}),
  });
  const {result, error} = await res.json();
  if (error) throw new Error(`${method}: ${error.message}`);
  return result;
}
const readAddress = async (to, selector) =>
  '0x' + (await rpc('eth_call', [{to, data: selector}, 'latest'])).slice(-40);
const compiled = (name) =>
  JSON.parse(readFileSync(join(ROOT, 'out', `${name}.sol`, `${name}.json`), 'utf8'))
    .deployedBytecode.object;
const same = (a, b) => String(a).toLowerCase() === String(b).toLowerCase();

// Selectors from `cast sig`.
const CHECKS = {
  async SettlementValidator(addr) {
    const escrow = await readAddress(addr, '0xe2fdcc17'); // escrow()
    const registry = await readAddress(addr, '0x4c6b993a'); // validationRegistry()
    if (!same(escrow, record.contracts.TaskEscrow)) return `bound to escrow ${escrow}`;
    if (!same(registry, net.erc8004.validationRegistry)) return `bound to registry ${registry}`;
    record.erc8004 = {...(record.erc8004 ?? {}), validationRegistry: net.erc8004.validationRegistry};
    return null;
  },
  async AgentAccountFactory(addr) {
    const token = await readAddress(addr, '0xfc0c546a'); // token()
    if (!same(token, record.contracts.PaymentToken)) return `bound to token ${token}`;
    const impl = await readAddress(addr, '0x5c60da1b'); // implementation()
    const code = await rpc('eth_getCode', [impl, 'latest']);
    if (!same(code, compiled('AgentAccount'))) {
      return `implementation ${impl} is not the compiled AgentAccount`;
    }
    return null;
  },
  async AgentDelegate(addr) {
    const code = await rpc('eth_getCode', [addr, 'latest']);
    return same(code, compiled('AgentDelegate')) ? null : 'runtime code is not the compiled AgentDelegate';
  },
};

const created = run.transactions.filter((t) => t.transactionType === 'CREATE' && CHECKS[t.contractName]);
if (created.length === 0) {
  console.error(`no recordable contract creations in ${runFile}`);
  process.exit(1);
}

// From here on, never process.exit(): on Windows it can fire while fetch's
// keep-alive socket is closing, and abort with a libuv assertion and exit
// code 127 instead of 1. Set the code and let the process end.
const problems = [];
for (const tx of created) {
  const receipt = run.receipts.find((r) => r.transactionHash === tx.hash);
  const problem =
    !receipt || Number(receipt.status) !== 1
      ? 'has no successful receipt'
      : await CHECKS[tx.contractName](tx.contractAddress);
  if (problem) problems.push(`${tx.contractName} ${tx.contractAddress} ${problem}`);
}

if (problems.length > 0) {
  for (const p of problems) console.error(`refusing: ${p}`);
  process.exitCode = 1;
} else {
  // Every check passed: write them all. No tx hash in the record — a 32-byte
  // hex there is indistinguishable from a key to the secret scan; the hashes
  // are printed for the commit message (broadcast/ is gitignored).
  for (const tx of created) {
    const previous = record.contracts[tx.contractName];
    record.contracts[tx.contractName] = tx.contractAddress;
    console.log(
      `${tx.contractName} ${tx.contractAddress} — checked (tx ${tx.hash})` +
        (previous && !same(previous, tx.contractAddress) ? `; replaces ${previous}` : ''),
    );
  }
  writeFileSync(target, JSON.stringify(record, null, 2) + '\n');
  console.log(`deployments/${chainId}.json updated`);
}
