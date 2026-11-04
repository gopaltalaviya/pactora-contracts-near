#!/usr/bin/env node
/**
 * Config integrity — runs in CI on every PR.
 *
 * The single-source-of-truth design (docs/08) only holds if the sources agree
 * with each other. This catches the failure it is designed to prevent: a
 * parameter added for one network and forgotten on another, which would
 * deploy with a zero value and nobody would notice until money moved.
 */

import {readFileSync, readdirSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => JSON.parse(readFileSync(join(ROOT, p), 'utf8'));

const errors = [];
const fail = (msg) => errors.push(msg);

const networks = read('config/networks.json');

// ── 1. networks.json shape ────────────────────────────────────────────────
const REQUIRED_NET_KEYS = [
  'chainId',
  'name',
  'shortName',
  'foundryAlias',
  'testnet',
  'nativeCurrency',
  'rpcUrls',
  'blockExplorerUrls',
  'confirmations',
  'paymentToken',
  'erc8004',
];

for (const [key, net] of Object.entries(networks)) {
  for (const k of REQUIRED_NET_KEYS) {
    if (!(k in net)) fail(`networks.json[${key}]: missing "${k}"`);
  }
  if (net.chainId !== Number(key)) {
    fail(`networks.json[${key}]: chainId field is ${net.chainId}, key is ${key}`);
  }
  if (!Array.isArray(net.rpcUrls) || net.rpcUrls.length === 0) {
    fail(`networks.json[${key}]: rpcUrls must be a non-empty array`);
  }
  if (net.paymentToken?.decimals !== 6) {
    fail(`networks.json[${key}]: paymentToken.decimals must be 6 (USDC-style)`);
  }

  // A production network must never silently deploy a mintable test token.
  if (net.testnet === false && net.paymentToken?.deployWithProtocol === true) {
    fail(`networks.json[${key}]: refusing to deploy MockUSDC on a non-testnet`);
  }

  // If we are NOT deploying ERC-8004 here, we must know where it lives.
  const e = net.erc8004 ?? {};
  if (e.deployWithProtocol === false) {
    for (const k of ['identityRegistry', 'reputationRegistry']) {
      if (!e[k]) fail(`networks.json[${key}]: erc8004.${k} required when deployWithProtocol=false`);
    }
  }
}

// ── 2. every network has a params file, and they agree on keys ────────────
const paramFiles = readdirSync(join(ROOT, 'config')).filter((f) =>
  /^params\.\d+\.json$/.test(f),
);

for (const chainId of Object.keys(networks)) {
  if (!paramFiles.includes(`params.${chainId}.json`)) {
    fail(`config/params.${chainId}.json is missing (declared in networks.json)`);
  }
}

const keysOf = (f) =>
  new Set(Object.keys(read(`config/${f}`)).filter((k) => !k.startsWith('_')));

let reference = null;
let referenceFile = null;
for (const f of paramFiles) {
  const ks = keysOf(f);
  if (!reference) {
    reference = ks;
    referenceFile = f;
    continue;
  }
  const missing = [...reference].filter((k) => !ks.has(k));
  const extra = [...ks].filter((k) => !reference.has(k));
  if (missing.length) fail(`config/${f}: missing ${missing.join(', ')} (present in ${referenceFile})`);
  if (extra.length) fail(`config/${f}: has ${extra.join(', ')} which ${referenceFile} lacks`);
}

// ── 3. parameter values must be sane ──────────────────────────────────────
for (const f of paramFiles) {
  const p = read(`config/${f}`);
  const chainId = f.match(/\d+/)[0];
  const isTestnet = networks[chainId]?.testnet;

  const big = (k) => BigInt(p[k]);

  if (big('fastPathMax') <= 0n) fail(`${f}: fastPathMax must be > 0`);
  if (big('minStake') <= 0n) fail(`${f}: minStake must be > 0`);
  if (p.protocolFeeBps < 0 || p.protocolFeeBps > 1000) {
    fail(`${f}: protocolFeeBps ${p.protocolFeeBps} outside 0..1000 (10%)`);
  }
  if (p.fastPathMinScore < 0 || p.fastPathMinScore > 100) {
    fail(`${f}: fastPathMinScore must be 0..100`);
  }
  if (big('minJobAmount') <= 0n) fail(`${f}: minJobAmount must be > 0 — dust would buy reviews`);
  if (big('minFee') >= big('minJobAmount')) fail(`${f}: minFee must be below minJobAmount`);
  if (big('fastPathMax') < big('minJobAmount')) fail(`${f}: fastPathMax below minJobAmount — no fast-path job could exist`);
  if (big('maxJobAmount') < big('minJobAmount')) fail(`${f}: maxJobAmount below minJobAmount — no escrow job could exist`);
  const fg = Number(p.feedbackGas);
  if (!(fg >= 100000 && fg <= 2000000)) fail(`${f}: feedbackGas must be 100000..2000000 (the contract's bounds)`);
  if (!(Number(p.bondBps) >= 0 && Number(p.bondBps) <= 10000)) fail(`${f}: bondBps must be 0..10000`);
  if (!(Number(p.disputeBondBps) >= 0 && Number(p.disputeBondBps) <= 10000)) fail(`${f}: disputeBondBps must be 0..10000`);
  if (big('minBond') > big('minStake')) fail(`${f}: minBond above minStake — no hireable worker could accept a job`);
  if (big('maxJobAmount') < big('fastPathMax')) fail(`${f}: maxJobAmount below fastPathMax — a direct payment could exceed what escrow allows`);
  if (!(p.adminDelaySeconds > 0)) fail(`${f}: adminDelaySeconds must be > 0`);
  for (const k of ['acceptWindowSeconds', 'workWindowSeconds', 'reviewWindowSeconds', 'disputeTimeoutSeconds']) {
    if (!(p[k] > 0)) fail(`${f}: ${k} must be > 0 — a zero window traps funds`);
  }
  if (p.confidenceFloor < 1) fail(`${f}: confidenceFloor must be >= 1`);

  // Mainnet must not inherit demo-tuned windows. A 10-minute auto-approve is
  // fine for a demo and indefensible against real funds.
  if (isTestnet === false) {
    if (p.reviewWindowSeconds < 3600) {
      fail(`${f}: mainnet reviewWindowSeconds ${p.reviewWindowSeconds} < 3600 — too short to dispute`);
    }
    if (p.disputeTimeoutSeconds < 86400) {
      fail(`${f}: mainnet disputeTimeoutSeconds ${p.disputeTimeoutSeconds} < 1 day — too short for an arbiter`);
    }
    if (p.adminDelaySeconds < 86400) {
      fail(`${f}: mainnet adminDelaySeconds ${p.adminDelaySeconds} < 1 day`);
    }
    if (big('minStake') < 100_000_000n) {
      fail(`${f}: mainnet minStake below 100 USDC — sybil resistance too weak`);
    }
  }
}

if (errors.length) {
  console.error('\n✗ Config integrity FAILED\n');
  for (const e of errors) console.error(`  • ${e}`);
  console.error('');
  process.exit(1);
}

console.log(
  `✓ Config integrity OK — ${Object.keys(networks).length} networks, ` +
    `${paramFiles.length} parameter files, ${reference.size} parameters each`,
);
