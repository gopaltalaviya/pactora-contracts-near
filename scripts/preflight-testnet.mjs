#!/usr/bin/env node
/**
 * Go/no-go before the testnet run. Read-only: it sends no transaction and
 * spends nothing.
 *
 *   node --env-file=.env scripts/preflight-testnet.mjs                   # testnet (the default)
 *   SANDBOX_KEY_FILE=... node scripts/preflight-testnet.mjs --network sandbox  # self-check on a real node
 *
 * Checks (scripts/lib/preflight.mjs, unit-tested in scripts/preflight.test.mjs):
 * the RPC is testnet; DEPLOYER_ACCOUNT_ID / DEPLOYER_SECRET_KEY are present and
 * well-formed; the account exists and the key is a FULL-ACCESS key on it; the
 * balance covers what is left of the README budget (25 NEAR for a full pass);
 * deployments/testnet.json is absent or consistent with the chain; the
 * committed wasm matches wasm/SHA256SUMS; config/params.testnet.json parses
 * and has dispute_grace; any COUNCIL_* / ARBITER_ACCOUNT_ID already set are
 * well-formed. The secret key is never printed.
 *
 * Exit 0 on GO only; 1 on NO-GO; 2 on a usage error.
 */
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {preflight, rpcFromUrl} from './lib/preflight.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const i = process.argv.indexOf('--network');
const slug = i >= 0 ? process.argv[i + 1] : 'testnet';

let rpcUrl = 'http://127.0.0.1:1';
try {
  const net = JSON.parse(readFileSync(join(ROOT, 'config', 'networks.json'), 'utf8'))[`near:${slug}`];
  if (slug === 'testnet') rpcUrl = process.env.RPC_URL_TESTNET || net.rpcUrls[0];
  else if (slug === 'sandbox') rpcUrl = process.env.SANDBOX_RPC || net.rpcUrls[0];
} catch {
  /* an unknown network is refused by the network check */
}

console.log(`preflight: ${slug === 'testnet' || slug === 'sandbox' ? `near:${slug}` : slug}, read-only (no transaction is sent)`);
const r = await preflight({slug, env: process.env, root: ROOT, rpc: rpcFromUrl(rpcUrl)});
const w = Math.max(...r.checks.map((c) => c.name.length));
for (const c of r.checks) console.log(`  ${c.status.padEnd(4)}  ${c.name.padEnd(w)}  ${c.detail}`);

// process.exitCode, not process.exit(): exiting with fetch sockets still open
// trips a libuv assertion on Windows and the exit code becomes 127.
if (!r.go) {
  console.log('NO-GO — fix every FAIL above and run the preflight again. Nothing was spent.');
  process.exitCode = slug === 'testnet' || slug === 'sandbox' ? 1 : 2;
} else {
  console.log('GO');
  if (slug === 'sandbox') console.log('(sandbox self-check only: the commands below are for testnet)');
  console.log('next, in order (README "Testnet"):');
  for (const cmd of r.next) console.log(`  ${cmd}`);
}
