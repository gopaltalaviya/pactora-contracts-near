#!/usr/bin/env node
/**
 * Delete the saved full-access testnet test accounts into the deployer (your
 * account), returning their NEAR.
 *
 *   node --env-file=.env scripts/sweep-testnet.mjs
 *
 * verify-near already sweeps at its start and end; this is for a run that
 * stopped half-way. An account that cannot be deleted keeps its key file in
 * .keys/testnet/.
 */
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {JsonRpcProvider} from 'near-api-js';
import {sweep} from './lib/throwaway.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const rootId = process.env.DEPLOYER_ACCOUNT_ID;
if (!rootId?.endsWith('.testnet')) {
  console.error('sweep-testnet: DEPLOYER_ACCOUNT_ID (a .testnet account) must be set in .env');
  process.exit(2);
}
const provider = new JsonRpcProvider({url: process.env.RPC_URL_TESTNET || 'https://rpc.testnet.near.org'});
const {chain_id} = await provider.viewNodeStatus();
if (chain_id !== 'testnet') {
  console.error(`sweep-testnet: the RPC is "${chain_id}", expected "testnet"`);
  process.exit(2);
}
const back = await sweep(ROOT, 'testnet', provider, rootId);
console.log(`sweep-testnet: about ${(Number(back / 10n ** 18n) / 1e6).toFixed(4)} NEAR returned to ${rootId}`);
