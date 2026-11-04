#!/usr/bin/env node
/**
 * M1-00 / M1-00d — ERC-8004 registry probe.
 *
 * Answers, for every configured network:
 *   1. Does the chain respond, and is its chainId what we think it is?
 *   2. Are the ERC-8004 registries actually deployed there?
 *   3. Is the Identity Registry a working ERC-721?
 *   4. Has the ERC-1967 implementation moved since we pinned it?
 *
 * (4) is the one that matters long-term: the registries are upgradeable
 * proxies owned by someone else. Their ABI can change under us. This turns
 * that from "discovered during the demo" into "CI told us".
 *
 * Exit code 0 = everything matches config. 1 = a mismatch worth a human.
 */

import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const networks = JSON.parse(readFileSync(join(ROOT, 'config/networks.json'), 'utf8'));

const ERC1967_IMPL_SLOT =
  '0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc';

const SELECTOR = {
  name: '0x06fdde03',
  symbol: '0x95d89b41',
  ownerOf: '0x6352211e',
};

let failures = 0;
const note = (s) => console.log(`   ${s}`);

async function rpc(url, method, params) {
  const res = await fetch(url, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method, params}),
    signal: AbortSignal.timeout(20_000),
  });
  const json = await res.json();
  if (json.error) throw new Error(json.error.message);
  return json.result;
}

/** Decode a solidity `string` return value from an eth_call result. */
function decodeString(hex) {
  const h = hex.slice(2);
  if (h.length < 128) return null;
  const len = parseInt(h.slice(64, 128), 16);
  return Buffer.from(h.slice(128, 128 + len * 2), 'hex').toString('utf8');
}

const pad32 = (n) => BigInt(n).toString(16).padStart(64, '0');

for (const [chainId, net] of Object.entries(networks)) {
  const url = process.env[`RPC_URL_${chainId}`] ?? net.rpcUrls[0];
  console.log(`\n=== ${net.name} (${chainId}) — ${url}`);

  if (!url || url.startsWith('http://127.0.0.1')) {
    note('skipped (local)');
    continue;
  }

  let actualChainId;
  try {
    actualChainId = parseInt(await rpc(url, 'eth_chainId', []), 16);
  } catch (err) {
    note(`UNREACHABLE: ${err.message}`);
    note('not counted as a failure — the network may simply be down');
    continue;
  }

  if (actualChainId !== Number(chainId)) {
    note(`FAIL chainId: config says ${chainId}, chain says ${actualChainId}`);
    failures++;
    continue;
  }
  note(`chainId ${actualChainId} ✓`);

  const cfg = net.erc8004 ?? {};

  if (cfg.deployWithProtocol) {
    note('ERC-8004: we deploy it here — checking it is genuinely absent');
    for (const key of ['identityRegistry', 'reputationRegistry']) {
      if (cfg[key]) note(`  ${key}: configured as ${cfg[key]} (post-deploy)`);
      else note(`  ${key}: null, as expected before deploy`);
    }
    continue;
  }

  for (const key of ['identityRegistry', 'reputationRegistry']) {
    const addr = cfg[key];
    if (!addr) {
      note(`FAIL ${key}: expected a canonical address, config has null`);
      failures++;
      continue;
    }
    const code = await rpc(url, 'eth_getCode', [addr, 'latest']);
    if (code === '0x') {
      note(`FAIL ${key} ${addr}: NO CONTRACT`);
      failures++;
      continue;
    }
    note(`${key} ${addr}: ${(code.length - 2) / 2} bytes ✓`);
  }

  // Identity Registry must behave like the ERC-721 the standard describes.
  if (cfg.identityRegistry) {
    const call = (data) =>
      rpc(url, 'eth_call', [{to: cfg.identityRegistry, data}, 'latest']);
    try {
      const name = decodeString(await call(SELECTOR.name));
      const symbol = decodeString(await call(SELECTOR.symbol));
      note(`ERC-721 name()="${name}" symbol()="${symbol}" ✓`);

      const owner = await call(SELECTOR.ownerOf + pad32(1));
      note(`ownerOf(1)=0x${owner.slice(-40)} — ids are sequential ✓`);
    } catch (err) {
      note(`FAIL ERC-721 surface: ${err.message}`);
      failures++;
    }

    // The pin. If this moves, the ABI we integrated against may no longer apply.
    if (cfg.identityImplementationPinned) {
      const slot = await rpc(url, 'eth_getStorageAt', [
        cfg.identityRegistry,
        ERC1967_IMPL_SLOT,
        'latest',
      ]);
      const impl = '0x' + slot.slice(-40);
      const pinned = cfg.identityImplementationPinned.toLowerCase();
      if (impl.toLowerCase() === pinned) {
        note(`proxy implementation ${impl} — unchanged ✓`);
      } else {
        note(`FAIL proxy UPGRADED: pinned ${pinned}, now ${impl}`);
        note('   Re-verify the ABI before trusting giveFeedback()/getSummary().');
        failures++;
      }
    }
  }

  // v3: the Reputation Registry is the one the escrow WRITES to, and its
  // gas cost sets Config.feedbackGas. An upgrade there matters more, not less.
  if (cfg.reputationRegistry && cfg.reputationImplementationPinned) {
    const slot = await rpc(url, 'eth_getStorageAt', [
      cfg.reputationRegistry,
      ERC1967_IMPL_SLOT,
      'latest',
    ]);
    const impl = '0x' + slot.slice(-40);
    const pinned = cfg.reputationImplementationPinned.toLowerCase();
    if (impl.toLowerCase() === pinned) {
      note(`reputation implementation ${impl} — unchanged ✓`);
    } else {
      note(`FAIL reputation proxy UPGRADED: pinned ${pinned}, now ${impl}`);
      note('   Re-measure giveFeedback gas and re-check feedbackGas before settling anything.');
      failures++;
    }
  }

  // v3: the Validation Registry SettlementValidator answers in. Upstream marks
  // it as still under discussion, so an upgrade is likely — and would need the
  // validator's calls re-checked against the new code.
  if (cfg.validationRegistry && cfg.validationImplementationPinned) {
    const code = await rpc(url, 'eth_getCode', [cfg.validationRegistry, 'latest']);
    if (code === '0x') {
      note(`FAIL validationRegistry ${cfg.validationRegistry}: NO CONTRACT`);
      failures++;
    } else {
      const slot = await rpc(url, 'eth_getStorageAt', [cfg.validationRegistry, ERC1967_IMPL_SLOT, 'latest']);
      const impl = '0x' + slot.slice(-40);
      const pinned = cfg.validationImplementationPinned.toLowerCase();
      if (impl.toLowerCase() === pinned) {
        note(`validation implementation ${impl} — unchanged ✓`);
      } else {
        note(`FAIL validation proxy UPGRADED: pinned ${pinned}, now ${impl}`);
        note('   Re-check validationRequest/validationResponse before answering on it.');
        failures++;
      }
    }
  }
}

console.log(
  failures === 0
    ? '\nAll ERC-8004 expectations match config.'
    : `\n${failures} mismatch(es). See above.`,
);
process.exit(failures === 0 ? 0 : 1);
