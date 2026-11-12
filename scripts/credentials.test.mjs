/**
 * Unit tests for mergeCredentials (scripts/lib/credentials.mjs): the deploy
 * script adds the credentials contract to deployments/<network>.json without
 * changing anything else. Run: npm run test:credentials
 */
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {test} from 'node:test';
import {fileURLToPath} from 'node:url';
import {mergeCredentials} from './lib/credentials.mjs';

const REPO = join(dirname(fileURLToPath(import.meta.url)), '..');
const SHA = 'a'.repeat(64);

test('adds Credentials and wasm.credentials, every other field unchanged', () => {
  const before = JSON.parse(readFileSync(join(REPO, 'deployments', 'testnet.json'), 'utf8'));
  const snapshot = structuredClone(before);
  const after = mergeCredentials(before, {accountId: 'credentials.gopalt.testnet', wasmSha256: SHA});
  assert.deepEqual(before, snapshot, 'the input is not mutated');
  assert.equal(after.contracts.Credentials, 'credentials.gopalt.testnet');
  assert.equal(after.wasm.credentials, SHA);
  const {Credentials, ...otherContracts} = after.contracts;
  const {credentials, ...otherWasm} = after.wasm;
  assert.ok(Credentials && credentials);
  assert.deepEqual(otherContracts, before.contracts);
  assert.deepEqual(otherWasm, before.wasm);
  for (const k of Object.keys(before)) if (k !== 'contracts' && k !== 'wasm') assert.deepEqual(after[k], before[k], k);
  assert.deepEqual(Object.keys(after), Object.keys(before), 'top-level key order kept');
});

test('a re-run replaces only the credentials entries', () => {
  const once = mergeCredentials({chainId: 'near:testnet', contracts: {Pactora: 'p'}, wasm: {pactora: 'x'}}, {accountId: 'c1', wasmSha256: SHA});
  const twice = mergeCredentials(once, {accountId: 'c2', wasmSha256: 'b'.repeat(64)});
  assert.deepEqual(twice, {chainId: 'near:testnet', contracts: {Pactora: 'p', Credentials: 'c2'}, wasm: {pactora: 'x', credentials: 'b'.repeat(64)}});
});

test('a file with no wasm object gets one', () => {
  assert.deepEqual(mergeCredentials({contracts: {}}, {accountId: 'c', wasmSha256: SHA}).wasm, {credentials: SHA});
});

test('refuses bad input', () => {
  assert.throws(() => mergeCredentials(null, {accountId: 'c', wasmSha256: SHA}));
  assert.throws(() => mergeCredentials({}, {accountId: 'c', wasmSha256: SHA}), /no contracts/);
  assert.throws(() => mergeCredentials({contracts: {}}, {accountId: '', wasmSha256: SHA}), /accountId/);
  assert.throws(() => mergeCredentials({contracts: {}}, {accountId: 'c', wasmSha256: 'nothex'}), /wasmSha256/);
});
