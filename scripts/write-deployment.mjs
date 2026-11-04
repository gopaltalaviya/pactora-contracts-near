#!/usr/bin/env node
/**
 * Generate deployments/<chainId>.json from the Foundry broadcast artifact.
 *
 * Deployment addresses are GENERATED, never hand-maintained. A hand-edited
 * address file is how a backend ends up talking to a contract nobody deployed.
 *
 *   node scripts/write-deployment.mjs <network-alias>
 */

import {readFileSync, writeFileSync, existsSync, readdirSync, mkdirSync} from 'node:fs';
import {execSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const alias = process.argv[2];

if (!alias) {
  console.error('usage: write-deployment.mjs <network-alias>   (e.g. monad_testnet)');
  process.exit(1);
}

const networks = JSON.parse(readFileSync(join(ROOT, 'config/networks.json'), 'utf8'));

const entry = Object.entries(networks).find(([, n]) => n.foundryAlias === alias);
if (!entry) {
  console.error(
    `unknown network alias "${alias}". Known: ` +
      Object.values(networks).map((n) => n.foundryAlias).join(', '),
  );
  process.exit(1);
}
const [chainIdStr, net] = entry;
const chainId = Number(chainIdStr);

// Foundry writes broadcast/<script>/<chainId>/run-latest.json
const broadcastDir = join(ROOT, 'broadcast', 'Deploy.s.sol', String(chainId));
const runFile = join(broadcastDir, 'run-latest.json');

if (!existsSync(runFile)) {
  console.error(`no broadcast artifact at ${runFile}`);
  console.error(`did \`forge script ... --rpc-url ${alias} --broadcast\` run?`);
  if (existsSync(join(ROOT, 'broadcast', 'Deploy.s.sol'))) {
    console.error(
      `chains present: ${readdirSync(join(ROOT, 'broadcast', 'Deploy.s.sol')).join(', ')}`,
    );
  }
  process.exit(1);
}

const run = JSON.parse(readFileSync(runFile, 'utf8'));

if (Number(run.chain) !== chainId) {
  console.error(`artifact chain ${run.chain} != expected ${chainId} — refusing to write`);
  process.exit(1);
}

// Collect every CREATE from this run.
const contracts = {};
let minBlock = Number.POSITIVE_INFINITY;

for (const tx of run.transactions ?? []) {
  if (tx.transactionType !== 'CREATE' && tx.transactionType !== 'CREATE2') continue;
  if (!tx.contractName || !tx.contractAddress) continue;
  contracts[tx.contractName] = tx.contractAddress;
}

for (const r of run.receipts ?? []) {
  const bn = Number(r.blockNumber);
  if (Number.isFinite(bn)) minBlock = Math.min(minBlock, bn);
}
if (!Number.isFinite(minBlock)) minBlock = 0;

if (Object.keys(contracts).length === 0) {
  console.error('no contract creations found in the broadcast artifact');
  process.exit(1);
}

// MockUSDC is the payment token wherever we deploy it; otherwise the
// canonical address from the registry. One name downstream either way.
if (contracts['MockUSDC']) {
  contracts['PaymentToken'] = contracts['MockUSDC'];
} else if (net.paymentToken.address) {
  contracts['PaymentToken'] = net.paymentToken.address;
}

// ERC-8004: our own deployment on chains where it is absent, the canonical
// addresses everywhere else. Resolved here so nothing downstream branches.
const erc8004 = {};
if (net.erc8004.deployWithProtocol) {
  // The reference implementation compiles under the Mock* names; downstream
  // only ever sees the resolved role, never the contract name.
  const id = contracts['MockIdentityRegistry'] ?? contracts['IdentityRegistry'];
  const rep = contracts['MockReputationRegistry'] ?? contracts['ReputationRegistry'];
  if (id) erc8004.identityRegistry = id;
  if (rep) erc8004.reputationRegistry = rep;
} else {
  if (net.erc8004.identityRegistry) erc8004.identityRegistry = net.erc8004.identityRegistry;
  if (net.erc8004.reputationRegistry) {
    erc8004.reputationRegistry = net.erc8004.reputationRegistry;
  }
  // v3: where SettlementValidator answers. Never deployed by us.
  if (net.erc8004.validationRegistry) {
    erc8004.validationRegistry = net.erc8004.validationRegistry;
  }
}

let commit = 'unknown';
try {
  commit = execSync('git rev-parse --short HEAD', {cwd: ROOT, stdio: ['ignore', 'pipe', 'ignore']})
    .toString()
    .trim();
} catch {
  // Not a git repo yet — fine. The field records provenance when available.
}

/** Forge's run timestamp, in ms whether it was written in s or ms. */
function toMillis(ts) {
  const n = Number(ts);
  if (!Number.isFinite(n) || n <= 0) return null;
  return n > 1e12 ? n : n * 1000;
}

const out = {
  chainId,
  // Forge writes `timestamp` in MILLISECONDS. Multiplying it by 1000 again
  // dated both deployments in the year 58698.
  deployedAt: new Date(toMillis(run.timestamp) ?? Date.now()).toISOString(),
  commit,
  startBlock: minBlock,
  contracts,
  ...(Object.keys(erc8004).length > 0 ? {erc8004} : {}),
};

const target = join(ROOT, 'deployments', `${chainId}.json`);

// Never lose the record of what is live. A redeploy (v3 beside a v2 that
// judges are still using) used to overwrite it; the previous record now moves
// to deployments/history/ first, named by its own commit and date.
if (existsSync(target)) {
  const prev = JSON.parse(readFileSync(target, 'utf8'));
  const same = JSON.stringify(prev.contracts) === JSON.stringify(out.contracts);
  if (!same) {
    const dir = join(ROOT, 'deployments', 'history');
    mkdirSync(dir, {recursive: true});
    const day = String(prev.deployedAt ?? '').slice(0, 10) || 'undated';
    const name = `${chainId}-${day}-${prev.commit ?? 'unknown'}.json`;
    writeFileSync(join(dir, name), readFileSync(target, 'utf8'));
    console.log(`archived the previous record to deployments/history/${name}`);
  }
}
writeFileSync(target, JSON.stringify(out, null, 2) + '\n');

console.log(`wrote deployments/${chainId}.json`);
console.log(`  startBlock ${minBlock}   commit ${commit}`);
for (const [k, v] of Object.entries(contracts)) console.log(`  ${k.padEnd(22)} ${v}`);
for (const [k, v] of Object.entries(erc8004)) console.log(`  erc8004.${k.padEnd(14)} ${v}`);
