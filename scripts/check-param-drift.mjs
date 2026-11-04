#!/usr/bin/env node
/**
 * Parameter drift — does the deployed chain still say what the config says?
 *
 *   make drift NETWORK=monad_testnet
 *   node scripts/check-param-drift.mjs monad_testnet
 *
 * `config/params.<chainId>.json` is the source of truth for protocol
 * parameters (docs/08). The contracts copy them in at deploy, and an admin
 * can change them afterwards. If the two disagree, the backend reports one
 * fee or fast-path threshold while the chain charges another — so this reads
 * every deployed value and compares it, using the same mapping as
 * `script/Deploy.s.sol`.
 *
 * `Makefile` has called this script since the first commit; until now it did
 * not exist.
 *
 * Reads only. Exit 0 on agreement, 1 on drift, 2 if it cannot check.
 */

import {readFileSync, existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => JSON.parse(readFileSync(join(ROOT, p), 'utf8'));

const alias = process.argv[2];
const networks = read('config/networks.json');
const entry = Object.values(networks).find((n) => n.foundryAlias === alias || String(n.chainId) === alias);
if (!entry) {
  console.error(
    `unknown network "${alias}" — expected one of: ${Object.values(networks)
      .map((n) => n.foundryAlias)
      .join(', ')}`,
  );
  process.exit(2);
}

const chainId = entry.chainId;
const deploymentPath = `deployments/${chainId}.json`;
if (!existsSync(join(ROOT, deploymentPath))) {
  console.error(`${deploymentPath} does not exist — nothing is deployed on ${entry.name} to compare against`);
  process.exit(2);
}

const params = read(`config/params.${chainId}.json`);
const deployment = read(deploymentPath);
const rpcUrl = process.env[`RPC_URL_${chainId}`] || entry.rpcUrls[0];

// Raw JSON-RPC, like verify-erc8004.mjs: this repo's tooling has no npm
// dependencies. Selectors from `cast sig`, not from memory.
const SELECTOR = {config: '0x79502c55', minStake: '0x375b3c0a', withdrawDelay: '0x0288a39c'};

async function call(to, data) {
  const res = await fetch(rpcUrl, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method: 'eth_call', params: [{to, data}, 'latest']}),
  });
  const body = await res.json();
  if (body.error) throw new Error(body.error.message);
  if (!body.result || body.result === '0x') throw new Error(`no code or no such function at ${to}`);
  return body.result;
}

/** Split an ABI-encoded return into 32-byte words, as unsigned integers. */
const words = (hex) => hex.slice(2).match(/.{64}/g).map((w) => BigInt(`0x${w}`));

const escrow = deployment.contracts.TaskEscrow;
const vault = deployment.contracts.StakeVault;

let onChain;
try {
  const [config, minStake, withdrawDelay] = await Promise.all([
    call(escrow, SELECTOR.config).then(words),
    call(vault, SELECTOR.minStake).then((r) => words(r)[0]),
    call(vault, SELECTOR.withdrawDelay).then((r) => words(r)[0]),
  ]);
  // TaskEscrow Config, in declaration order (v3 added the last six).
  // fastPathMinScore is not on chain — it is API policy, read from the params file.
  const [
    fastPathMax,
    minJobAmount,
    minFee,
    protocolFeeBps,
    reviewWindow,
    maxAcceptWindow,
    maxWorkWindow,
    disputeTimeout,
    maxJobAmount,
    feedbackGas,
    bondBps,
    minBond,
    disputeBondBps,
    minDisputeBond,
  ] = config;
  onChain = {
    fastPathMax,
    minJobAmount,
    minFee,
    protocolFeeBps,
    reviewWindow,
    maxAcceptWindow,
    maxWorkWindow,
    disputeTimeout,
    maxJobAmount,
    feedbackGas,
    bondBps,
    minBond,
    disputeBondBps,
    minDisputeBond,
    minStake,
    withdrawDelay,
  };
} catch (err) {
  console.error(`could not read the deployed contracts on ${entry.name}: ${err.message}`);
  process.exit(2);
}

// Deployed name → the value Deploy.s.sol derived it from.
const expected = {
  fastPathMax: BigInt(params.fastPathMax),
  minJobAmount: BigInt(params.minJobAmount),
  minFee: BigInt(params.minFee),
  protocolFeeBps: BigInt(params.protocolFeeBps),
  reviewWindow: BigInt(params.reviewWindowSeconds),
  maxAcceptWindow: BigInt(params.acceptWindowSeconds) * 10n,
  maxWorkWindow: BigInt(params.workWindowSeconds) * 10n,
  disputeTimeout: BigInt(params.disputeTimeoutSeconds),
  maxJobAmount: BigInt(params.maxJobAmount),
  feedbackGas: BigInt(params.feedbackGas),
  bondBps: BigInt(params.bondBps),
  minBond: BigInt(params.minBond),
  disputeBondBps: BigInt(params.disputeBondBps),
  minDisputeBond: BigInt(params.minDisputeBond),
  minStake: BigInt(params.minStake),
  withdrawDelay: BigInt(params.withdrawDelaySeconds),
};

let drift = 0;
console.log(`  ${entry.name} (${chainId}) — TaskEscrow ${escrow}, StakeVault ${vault}`);
for (const [name, want] of Object.entries(expected)) {
  const got = BigInt(onChain[name]);
  if (got === want) {
    console.log(`  ✓ ${name.padEnd(17)} ${got}`);
  } else {
    drift++;
    console.log(`  ✗ ${name.padEnd(17)} chain ${got}, config ${want}`);
  }
}

console.log(drift ? `\n${drift} parameter(s) drifted\n` : '\nno drift\n');
// exitCode, not exit(): exiting while fetch still holds its sockets aborts
// the process on Windows (0xC0000409) and the exit code is lost.
process.exitCode = drift ? 1 : 0;
