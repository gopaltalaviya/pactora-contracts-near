#!/usr/bin/env node
/**
 * Launch check — is this deployment safe to open to real users?
 *
 *   node scripts/check-launch.mjs <network> --deployer 0x… [--admin-safe 0x…]
 *   make check-launch NETWORK=monad DEPLOYER_ADDRESS=0x… ADMIN_SAFE=0x…
 *
 * docs/20 §3–§4 as one command. It reads `deployments/<chainId>.json` and
 * asks the chain, for every contract:
 *
 * - the deployer holds NO role, and no admin transfer is still pending;
 * - the admin, arbiter, pauser, fee and slash recipients are contracts (Safes),
 *   and the admin is the Safe named, if one is named;
 * - every contract is wired to the others and to config/networks.json — the
 *   payment token and its decimals, the ERC-8004 registries, the vault's
 *   locker, the client writer, the validator;
 * - the escrow is not paused;
 * - the parameters equal config/params.<chainId>.json (check-param-drift.mjs).
 *
 * The registries' implementations are pinned by `make verify-erc8004`.
 *
 * Reads only — it never sends a transaction. Exit 0 when every check passes,
 * 1 when one fails, 2 when it cannot check.
 */

import {existsSync, readFileSync} from 'node:fs';
import {spawnSync} from 'node:child_process';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => JSON.parse(readFileSync(join(ROOT, p), 'utf8'));

const args = process.argv.slice(2);
const flag = (name) => {
  const i = args.indexOf(name);
  return i === -1 ? undefined : args[i + 1];
};
const alias = args[0];
const deployer = (flag('--deployer') ?? process.env.DEPLOYER_ADDRESS ?? '').toLowerCase();
const adminSafe = (flag('--admin-safe') ?? process.env.ADMIN_SAFE ?? '').toLowerCase();
const isAddress = (a) => /^0x[0-9a-f]{40}$/.test(a);

const networks = read('config/networks.json');
const net = Object.values(networks).find((n) => n.foundryAlias === alias || String(n.chainId) === alias);
if (!net || !isAddress(deployer) || (adminSafe && !isAddress(adminSafe))) {
  console.error('usage: check-launch.mjs <network> --deployer 0x… [--admin-safe 0x…]');
  process.exit(2);
}
const deploymentPath = `deployments/${net.chainId}.json`;
if (!existsSync(join(ROOT, deploymentPath))) {
  console.error(`${deploymentPath} does not exist — nothing deployed on ${net.name}`);
  process.exit(2);
}
const d = read(deploymentPath);
const c = Object.fromEntries(Object.entries(d.contracts).map(([k, v]) => [k, v.toLowerCase()]));
const rpcUrl = process.env[`RPC_URL_${net.chainId}`] || net.rpcUrls[0];

// Raw JSON-RPC, like check-param-drift.mjs: no npm dependencies. Selectors
// and role ids from `cast sig` / `cast keccak`, not from memory.
const SEL = {
  hasRole: '0x91d14854',
  defaultAdmin: '0x84ef8ffc',
  pendingDefaultAdmin: '0xcf6eefb7',
  token: '0xfc0c546a',
  identityRegistry: '0x134e18f4',
  reputationRegistry: '0xc8db44e3',
  stakeVault: '0x2d788333',
  feeRecipient: '0x46904840',
  clientWriter: '0x09d63dc0',
  slashRecipient: '0xda805355',
  escrow: '0xe2fdcc17',
  validationRegistry: '0x4c6b993a',
  decimals: '0x313ce567',
  paused: '0x5c975abb',
  isLocker: '0x2ec63d7c',
  lockerCount: '0xccd96eb6',
  getRoleMemberCount: '0xca15c873',
  getRoleMember: '0x9010d07c',
};
const ROLE = {
  DEFAULT_ADMIN: '0x' + '0'.repeat(64),
  CONFIGURER: '0xf45c97b23e2beeefda80e1ce5cb3e234aa7b6888ad5cbabb75bfd654dd8e102d',
  ARBITER: '0xbb08418a67729a078f87bbc8d02a770929bb68f5bfdf134ae2ead6ed38e2f4ae',
  PAUSER: '0x65d7a28e3265b37a6474929f336521b332c1681b933f6cb9f3376673440d862a',
  SLASHER: '0x12b42e8a160f6064dc959c6f251e3af0750ad213dbecf573b4710d67d6c28e39',
};

async function rpc(method, params) {
  const res = await fetch(rpcUrl, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method, params}),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${method}: ${body.error.message}`);
  return body.result;
}
const word = (hex, i = 0) => hex.slice(2 + 64 * i, 2 + 64 * (i + 1));
const pad = (a) => a.replace(/^0x/, '').padStart(64, '0');
const call = (to, data) => rpc('eth_call', [{to, data}, 'latest']);
const addr = async (to, sel) => '0x' + word(await call(to, sel)).slice(24);
const uint = async (to, sel) => BigInt('0x' + word(await call(to, sel)));
const bool = async (to, data) => BigInt('0x' + word(await call(to, data))) === 1n;
const hasRole = (to, role, who) => bool(to, SEL.hasRole + pad(role) + pad(who));
const hasCode = async (a) => (await rpc('eth_getCode', [a, 'latest'])).length > 2;
// Every holder of a role, not just "does X hold it": a holder planted during
// the hand-over window is invisible to hasRole checks (audit F2).
async function membersOf(to, role) {
  const n = await uint(to, SEL.getRoleMemberCount + pad(role));
  const out = [];
  for (let i = 0n; i < n; i++) {
    out.push(await addr(to, SEL.getRoleMember + pad(role) + i.toString(16).padStart(64, '0')));
  }
  return out;
}

let failed = 0;
const check = (ok, what, detail = '') => {
  console.log(`  ${ok ? '✓' : '✗'} ${what}${detail ? ` — ${detail}` : ''}`);
  if (!ok) failed++;
};

const escrow = c.TaskEscrow;
const vault = c.StakeVault;
console.log(`${net.name} (${net.chainId}) — ${deploymentPath}, deployer ${deployer}`);

try {
  const chainId = BigInt(await rpc('eth_chainId', []));
  if (chainId !== BigInt(net.chainId)) throw new Error(`RPC is chain ${chainId}, not ${net.chainId}`);

  console.log('\nThe deployer holds nothing');
  for (const [name, role] of Object.entries(ROLE)) {
    if (name === 'SLASHER') continue;
    check(!(await hasRole(escrow, role, deployer)), `TaskEscrow ${name}`);
  }
  for (const name of ['DEFAULT_ADMIN', 'CONFIGURER', 'SLASHER']) {
    check(!(await hasRole(vault, ROLE[name], deployer)), `StakeVault ${name}`);
  }
  for (const [label, to] of [['TaskEscrow', escrow], ['StakeVault', vault]]) {
    const pending = '0x' + word(await call(to, SEL.pendingDefaultAdmin)).slice(24);
    check(/^0x0{40}$/.test(pending), `${label}: no admin transfer pending`, pending);
  }

  console.log('\nEvery role is held by a contract (a Safe)');
  const holders = {
    'TaskEscrow admin': await addr(escrow, SEL.defaultAdmin),
    'StakeVault admin': await addr(vault, SEL.defaultAdmin),
    'fee recipient': await addr(escrow, SEL.feeRecipient),
    'slash recipient': await addr(vault, SEL.slashRecipient),
  };
  for (const [what, who] of Object.entries(holders)) check(await hasCode(who), what, who);
  if (adminSafe) {
    check(holders['TaskEscrow admin'] === adminSafe, 'TaskEscrow admin is ADMIN_SAFE');
    check(holders['StakeVault admin'] === adminSafe, 'StakeVault admin is ADMIN_SAFE');
    check(await hasRole(escrow, ROLE.CONFIGURER, adminSafe), 'ADMIN_SAFE configures TaskEscrow');
    check(await hasRole(vault, ROLE.CONFIGURER, adminSafe), 'ADMIN_SAFE configures StakeVault');
  }
  for (const [env, role] of [['ARBITER_ADDRESS', 'ARBITER'], ['PAUSER_ADDRESS', 'PAUSER']]) {
    const who = (process.env[env] ?? '').toLowerCase();
    if (!isAddress(who)) {
      check(false, `${role} holder`, `set ${env} to check who holds it`);
      continue;
    }
    check((await hasRole(escrow, ROLE[role], who)) && (await hasCode(who)), `${role} held by a contract`, who);
  }

  console.log('\nNobody else holds a role');
  const arbiter = (process.env.ARBITER_ADDRESS ?? '').toLowerCase();
  const pauser = (process.env.PAUSER_ADDRESS ?? '').toLowerCase();
  const admin = holders['TaskEscrow admin'];
  // [contract, role, who may hold it, must someone?]
  const expected = [
    ['TaskEscrow', escrow, 'DEFAULT_ADMIN', [admin], true],
    ['TaskEscrow', escrow, 'CONFIGURER', [admin], true],
    ['TaskEscrow', escrow, 'ARBITER', [arbiter], true],
    ['TaskEscrow', escrow, 'PAUSER', [pauser, admin], true],
    ['StakeVault', vault, 'DEFAULT_ADMIN', [holders['StakeVault admin']], true],
    ['StakeVault', vault, 'CONFIGURER', [holders['StakeVault admin']], true],
    ['StakeVault', vault, 'SLASHER', [], false],
  ];
  for (const [label, to, name, allowed, required] of expected) {
    const members = await membersOf(to, ROLE[name]);
    const strangers = members.filter((m) => !allowed.includes(m));
    check(
      strangers.length === 0 && (!required || members.length > 0),
      `${label} ${name}: ${members.length === 0 ? 'nobody' : members.join(', ')}`,
      strangers.length ? `not expected: ${strangers.join(', ')}` : '',
    );
  }
  const lockers = await uint(vault, SEL.lockerCount);
  check(lockers === 1n, 'the escrow is the only locker', `${lockers} locker(s)`);

  console.log('\nWired to each other and to config');
  const token = net.paymentToken.address?.toLowerCase() ?? c.PaymentToken;
  check((await addr(escrow, SEL.token)) === token, 'TaskEscrow pays in the configured token', token);
  check((await addr(vault, SEL.token)) === token, 'StakeVault bonds in the same token');
  check((await uint(token, SEL.decimals)) === BigInt(net.paymentToken.decimals), `token has ${net.paymentToken.decimals} decimals`);
  const reg = net.erc8004;
  if (!reg.deployWithProtocol) {
    check((await addr(escrow, SEL.identityRegistry)) === reg.identityRegistry.toLowerCase(), 'identity registry is the canonical one');
    check((await addr(escrow, SEL.reputationRegistry)) === reg.reputationRegistry.toLowerCase(), 'reputation registry is the canonical one');
  }
  check((await addr(vault, SEL.identityRegistry)) === (await addr(escrow, SEL.identityRegistry)), 'vault and escrow share the identity registry');
  check((await addr(escrow, SEL.stakeVault)) === vault, 'escrow uses this vault');
  check(await bool(vault, SEL.isLocker + pad(escrow)), 'the vault lets the escrow lock bonds');
  check((await addr(escrow, SEL.clientWriter)) === c.ClientReputationWriter, 'client reviews come from the client writer');
  check((await addr(c.ClientReputationWriter, SEL.escrow)) === escrow, 'the client writer serves this escrow');
  if (c.SettlementValidator) {
    check((await addr(c.SettlementValidator, SEL.escrow)) === escrow, 'the validator answers for this escrow');
    if (reg.validationRegistry) {
      check(
        (await addr(c.SettlementValidator, SEL.validationRegistry)) === reg.validationRegistry.toLowerCase(),
        'the validator answers the configured Validation Registry',
      );
    }
  }
  check(!(await bool(escrow, SEL.paused)), 'the escrow is not paused');
} catch (err) {
  console.error(`could not check ${net.name}: ${err.message}`);
  process.exit(2);
}

console.log('\nParameters');
const drift = spawnSync(process.execPath, [join(ROOT, 'scripts/check-param-drift.mjs'), String(net.chainId)], {
  stdio: 'inherit',
});
if (drift.status === 2) process.exit(2);
check(drift.status === 0, 'on-chain parameters equal config/params');

console.log(failed ? `\n✗ ${failed} check(s) failed — not ready to launch` : '\n✓ ready: every launch check passed');
process.exit(failed ? 1 : 0);
