/**
 * The testnet go/no-go checks (scripts/preflight-testnet.mjs), read-only.
 *
 * `preflight({slug, env, root, rpc})` sends no transaction: it reads the env,
 * the repo's config/, wasm/ and deployments/, and asks the RPC only views
 * (status, view_account, view_access_key, contract views). `rpc` is the small
 * interface below so the checks run against a fake in the unit tests
 * (scripts/preflight.test.mjs); `rpcFromUrl` is the real one.
 *
 * No check ever puts a secret key in its message.
 */
import {createHash} from 'node:crypto';
import {existsSync, readFileSync} from 'node:fs';
import {join} from 'node:path';
import {JsonRpcProvider, KeyPair} from 'near-api-js';
import {FACTORY_TESTNET, codeHash} from './dao.mjs';

/** README "Testnet" budget, per step still to run (8 + 9 + 8 = 25 for a full pass). */
export const BUDGET = {createDao: 8, deploy: 9, verify: 8};
const YOCTO = 10n ** 24n;
/** Storage staking: 1e19 yoctoNEAR per byte is locked and cannot be spent. */
const STORAGE_PRICE_PER_BYTE = 10n ** 19n;

/** The README "Testnet" commands, in order. */
export const COMMANDS = {
  createDao: 'node --env-file=.env scripts/create-dao.mjs --network testnet   # council + DAO; ids and keys go to .env only',
  deploy: 'node --env-file=.env scripts/deploy.mjs --network testnet       # writes deployments/testnet.json',
  verify: 'node --env-file=.env scripts/verify-near.mjs --network testnet  # every flow, the DAO ruling included',
};

/** NEAR account id rules (nomicon): 2-64 chars, lowercase parts joined by '.', '-' or '_' between alphanumerics. */
export const isAccountId = (s) => typeof s === 'string' && s.length >= 2 && s.length <= 64 && /^(([a-z\d]+[-_])*[a-z\d]+\.)*([a-z\d]+[-_])*[a-z\d]+$/.test(s);

/** null if `s` is a usable ed25519 secret key, else why not. Never echoes the key. */
export function keyProblem(s) {
  if (!s) return 'is not set';
  if (!s.startsWith('ed25519:')) return 'does not start with "ed25519:"';
  if (!/^ed25519:[1-9A-HJ-NP-Za-km-z]+$/.test(s)) return 'is not a valid ed25519 secret key (not base58)';
  try {
    KeyPair.fromString(s).getPublicKey();
    return null;
  } catch {
    return 'is not a valid ed25519 secret key';
  }
}
const publicKeyOf = (s) => KeyPair.fromString(s).getPublicKey().toString();

/** wasm/SHA256SUMS as {file: sha256hex}, parsed exactly as scripts/deploy.mjs does. */
export function readSums(root) {
  return Object.fromEntries(
    readFileSync(join(root, 'wasm', 'SHA256SUMS'), 'utf8')
      .trim()
      .split('\n')
      .map((l) => l.trim())
      .map((l) => l.split(/\s+\*?/).reverse()),
  );
}
export const sha256 = (b) => createHash('sha256').update(b).digest('hex');
const near = (y) => `${(Number(y / 10n ** 18n) / 1e6).toFixed(4)} NEAR`;

/** The real RPC, views only. `account`/`accessKey` answer null for "does not exist", and throw on anything else. */
export function rpcFromUrl(url) {
  const provider = new JsonRpcProvider({url});
  const missing = (e) => /does not exist|UNKNOWN_ACCOUNT|UNKNOWN_ACCESS_KEY|access key .* not found/i.test(`${e?.type ?? ''} ${e?.message ?? e}`);
  return {
    url,
    chainId: async () => (await provider.viewNodeStatus()).chain_id,
    account: (id) => provider.query({request_type: 'view_account', account_id: id, finality: 'final'}).catch((e) => (missing(e) ? null : Promise.reject(e))),
    accessKey: (id, pk) =>
      provider
        .query({request_type: 'view_access_key', account_id: id, public_key: pk, finality: 'final'})
        .then((k) => k.permission)
        .catch((e) => (missing(e) ? null : Promise.reject(e))),
    view: (contractId, method, args = {}) => provider.callFunction({contractId, method, args, blockQuery: {finality: 'final'}}),
  };
}

/**
 * Run every check. Answers {checks: [{name, status: 'ok'|'FAIL'|'SKIP', detail}], go, next, rpcUrl}.
 * SKIP means a check could not run because one it depends on failed; it is not GO.
 */
export async function preflight({slug, env, root, rpc}) {
  const checks = [];
  const add = (name, status, detail) => checks.push({name, status, detail});
  const ok = (name, detail) => add(name, 'ok', detail);
  const bad = (name, detail) => add(name, 'FAIL', detail);
  const skip = (name, why) => add(name, 'SKIP', `not checked: ${why}`);
  const done = (next = []) => ({checks, go: checks.length > 0 && checks.every((c) => c.status === 'ok'), next});

  // 1. network
  if (slug !== 'testnet' && slug !== 'sandbox') {
    bad('network', `"${slug}" refused: this preflight is for testnet only (sandbox for its own self-check); mainnet is refused by every script`);
    return done();
  }
  const chainId = `near:${slug}`;
  let net;
  try {
    net = JSON.parse(readFileSync(join(root, 'config', 'networks.json'), 'utf8'))[chainId];
  } catch (e) {
    bad('network', `config/networks.json: ${e.message}`);
    return done();
  }
  if (!net) {
    bad('network', `${chainId} is not in config/networks.json`);
    return done();
  }
  let rpcUp = false;
  const at = rpc.url ? `RPC ${rpc.url}` : 'RPC';
  try {
    const got = await rpc.chainId();
    if (got !== net.nearNetworkId) bad('network', `${at} is "${got}", expected "${net.nearNetworkId}"`);
    else {
      rpcUp = true;
      ok('network', `${at} answers chain_id "${got}"`);
    }
  } catch (e) {
    bad('network', `${at} unreachable: ${String(e.message ?? e).slice(0, 160)}`);
  }

  // 2. deployer id and key (sandbox: from SANDBOX_KEY_FILE, the way verify-near reads it)
  let deployerId;
  let secret;
  let idSource = 'DEPLOYER_ACCOUNT_ID';
  let keySource = 'DEPLOYER_SECRET_KEY';
  if (slug === 'sandbox') {
    idSource = keySource = 'SANDBOX_KEY_FILE';
    const f = env.SANDBOX_KEY_FILE;
    if (f && existsSync(f)) {
      try {
        const k = JSON.parse(readFileSync(f, 'utf8'));
        deployerId = k.account_id;
        secret = k.secret_key;
      } catch {
        /* reported below as missing */
      }
    }
  } else {
    deployerId = env.DEPLOYER_ACCOUNT_ID;
    secret = env.DEPLOYER_SECRET_KEY;
  }
  let idOk = false;
  if (!deployerId) bad('deployer id', `${idSource === 'SANDBOX_KEY_FILE' ? 'SANDBOX_KEY_FILE must name the sandbox validator_key.json' : 'DEPLOYER_ACCOUNT_ID is not set (in .env)'}`);
  else if (!isAccountId(deployerId)) bad('deployer id', `"${deployerId}" is not a valid NEAR account id`);
  else if (slug === 'testnet' && !deployerId.endsWith('.testnet')) bad('deployer id', `${deployerId} is not a .testnet account`);
  else {
    idOk = true;
    ok('deployer id', `${deployerId} (from ${idSource})`);
  }
  const kp = keyProblem(secret);
  const keyOk = kp === null;
  if (keyOk) ok('deployer key', `${keySource} is an ed25519 key; public half ${publicKeyOf(secret)}`);
  else bad('deployer key', `${keySource === 'SANDBOX_KEY_FILE' ? 'the secret_key in SANDBOX_KEY_FILE' : 'DEPLOYER_SECRET_KEY'} ${kp}`);

  // 3. account exists, 4. key is full access on it
  let account = null;
  if (!rpcUp || !idOk) skip('account', !rpcUp ? 'no usable RPC' : 'no valid deployer id');
  else {
    try {
      account = await rpc.account(deployerId);
      if (account) ok('account', `${deployerId} exists on ${slug}`);
      else bad('account', `${deployerId} does not exist on ${slug} — create and fund it first`);
    } catch (e) {
      bad('account', `view_account ${deployerId}: ${String(e.message ?? e).slice(0, 160)}`);
    }
  }
  if (!account || !keyOk) skip('access key', !account ? 'no deployer account' : 'no valid deployer key');
  else {
    const pk = publicKeyOf(secret);
    try {
      const perm = await rpc.accessKey(deployerId, pk);
      if (perm === null) bad('access key', `${pk} is not a key of ${deployerId} — wrong ${keySource} for this account`);
      else if (perm === 'FullAccess') ok('access key', `${pk} is a full-access key of ${deployerId}`);
      else bad('access key', `${pk} is a function-call key on ${deployerId}; deploy and create-dao need a full-access key`);
    } catch (e) {
      bad('access key', `view_access_key: ${String(e.message ?? e).slice(0, 160)}`);
    }
  }

  // 5. deployments/<slug>.json: absent (fresh run) or consistent (re-run)
  const depFile = join(root, 'deployments', `${slug}.json`);
  const deployedAlready = existsSync(depFile);
  let sums = null;
  try {
    sums = readSums(root);
  } catch {
    /* reported by the wasm check */
  }
  const pactoraId = idOk ? `pactora.${deployerId}` : null;
  const musdcId = idOk ? `musdc.${deployerId}` : null;
  if (!rpcUp || !idOk) skip('deployment', !rpcUp ? 'no usable RPC' : 'no valid deployer id');
  else if (!deployedAlready) {
    try {
      const taken = [];
      for (const id of [pactoraId, musdcId]) if (await rpc.account(id)) taken.push(id);
      if (taken.length) bad('deployment', `no deployments/${slug}.json, but ${taken.join(' and ')} already exists — deploy.mjs refuses to overwrite`);
      else ok('deployment', `no deployments/${slug}.json and ${pactoraId}, ${musdcId} are free: a fresh deploy`);
    } catch (e) {
      bad('deployment', `view_account: ${String(e.message ?? e).slice(0, 160)}`);
    }
  } else {
    const why = await deploymentProblem({slug, env, root, rpc, depFile, chainId, pactoraId, musdcId, sums});
    if (why) bad('deployment', `deployments/${slug}.json: ${why}`);
    else ok('deployment', `deployments/${slug}.json is consistent with the chain (a re-run: deploy is done)`);
  }

  // 6. balance against what is left of the README budget
  const arbiterSet = slug === 'sandbox' || !!env.ARBITER_ACCOUNT_ID;
  const need = (arbiterSet ? 0 : BUDGET.createDao) + (deployedAlready ? 0 : BUDGET.deploy) + BUDGET.verify;
  if (!account) skip('balance', 'no deployer account');
  else {
    const locked = BigInt(account.storage_usage ?? 0) * STORAGE_PRICE_PER_BYTE;
    const free = BigInt(account.amount) - locked;
    const want = BigInt(need) * YOCTO;
    const steps = [!arbiterSet && 'create-dao', !deployedAlready && 'deploy', 'verify-near'].filter(Boolean).join(' + ');
    if (free >= want) ok('balance', `${near(free)} spendable, budget ${need} NEAR (${steps})`);
    else bad('balance', `${near(free)} spendable, budget ${need} NEAR (${steps}): short by ${near(want - free)}`);
  }

  // 7. the committed wasm, as deploy.mjs checks it
  if (!sums) bad('wasm', 'wasm/SHA256SUMS is missing or unreadable');
  else {
    const notes = [];
    let wasmBad = null;
    for (const [envName, file] of [
      ['PACTORA_WASM', 'pactora.wasm'],
      ['MOCK_USDC_WASM', 'mock_usdc.wasm'],
    ]) {
      // `||`, as deploy.mjs: an empty line in .env is "", not an override.
      const override = env[envName];
      const p = override || join(root, 'wasm', file);
      if (!existsSync(p)) {
        wasmBad = `${envName}: no wasm at ${p}`;
        break;
      }
      const h = sha256(readFileSync(p));
      if (override) notes.push(`${envName} override ${h.slice(0, 12)}… (not checked against SHA256SUMS)`);
      else if (h !== sums[file]) {
        wasmBad = `wasm/${file} does not match wasm/SHA256SUMS (${h.slice(0, 12)}… vs ${String(sums[file]).slice(0, 12)}…)`;
        break;
      } else notes.push(`${file} ${h.slice(0, 12)}…`);
    }
    if (wasmBad) bad('wasm', wasmBad);
    else ok('wasm', `matches wasm/SHA256SUMS: ${notes.join(', ')}`);
  }

  // 8. params
  const paramsFile = `config/params.${slug}.json`;
  try {
    const p = JSON.parse(readFileSync(join(root, 'config', `params.${slug}.json`), 'utf8'));
    const g = p?.onchain?.dispute_grace;
    if (!p?.onchain) bad('params', `${paramsFile} has no "onchain" object`);
    else if (g === undefined) bad('params', `${paramsFile} has no onchain.dispute_grace`);
    else if (!Number.isInteger(g) || g <= 0) bad('params', `${paramsFile} onchain.dispute_grace is ${JSON.stringify(g)}, expected a positive number of seconds`);
    else ok('params', `${paramsFile} parses; dispute_grace ${g} s`);
  } catch (e) {
    bad('params', `${paramsFile} does not parse: ${String(e.message ?? e).slice(0, 120)}`);
  }

  // 9. COUNCIL_* / ARBITER already in the env (scripts/create-dao.mjs)
  const councilWhy = await councilProblem({slug, env, rpc, rpcUp, deployerId: idOk ? deployerId : null});
  const anySet = ['COUNCIL_1_ACCOUNT_ID', 'COUNCIL_1_SECRET_KEY', 'COUNCIL_2_ACCOUNT_ID', 'COUNCIL_2_SECRET_KEY', 'ARBITER_ACCOUNT_ID'].filter((n) => env[n]);
  if (councilWhy) bad('council/arbiter', councilWhy);
  else ok('council/arbiter', anySet.length ? `${anySet.join(', ')} well-formed` : 'none set yet; create-dao creates them');

  const next = [...(slug === 'testnet' && !env.ARBITER_ACCOUNT_ID ? [COMMANDS.createDao] : []), ...(deployedAlready ? [] : [COMMANDS.deploy]), COMMANDS.verify];
  return done(next);
}

/** Why an existing deployments/<slug>.json does not match this env and the chain, or null. */
async function deploymentProblem({slug, env, root, rpc, depFile, chainId, pactoraId, musdcId, sums}) {
  let d;
  try {
    d = JSON.parse(readFileSync(depFile, 'utf8'));
  } catch (e) {
    return `does not parse: ${e.message}`;
  }
  if (d.chainId !== chainId) return `chainId "${d.chainId}", expected "${chainId}"`;
  const c = d.contracts ?? {};
  if (c.Pactora !== pactoraId) return `Pactora is ${c.Pactora}, expected ${pactoraId} (deploy.mjs names it after the deployer)`;
  if (c.PaymentToken !== musdcId) return `PaymentToken is ${c.PaymentToken}, expected ${musdcId}`;
  if (slug === 'testnet') {
    if (env.ARBITER_ACCOUNT_ID && c.Arbiter !== env.ARBITER_ACCOUNT_ID) return `Arbiter is ${c.Arbiter}, but ARBITER_ACCOUNT_ID is ${env.ARBITER_ACCOUNT_ID}`;
    if (sums && d.wasm?.pactora !== sums['pactora.wasm']) return `wasm.pactora ${String(d.wasm?.pactora).slice(0, 12)}… is not the committed build in wasm/SHA256SUMS`;
    if (sums && d.wasm?.mockUsdc !== sums['mock_usdc.wasm']) return `wasm.mockUsdc ${String(d.wasm?.mockUsdc).slice(0, 12)}… is not the committed build in wasm/SHA256SUMS`;
  }
  try {
    const acct = await rpc.account(c.Pactora);
    if (!acct) return `${c.Pactora} does not exist on ${slug}`;
    const want = codeHash(readFileSync(join(root, 'wasm', 'pactora.wasm')));
    if (acct.code_hash !== want) return `${c.Pactora} runs code ${acct.code_hash}, not wasm/pactora.wasm (${want})`;
    const roles = await rpc.view(c.Pactora, 'get_roles');
    if (roles.token !== c.PaymentToken || roles.arbiter !== c.Arbiter) return `get_roles on chain is token ${roles.token}, arbiter ${roles.arbiter}; the file says ${c.PaymentToken}, ${c.Arbiter}`;
    const params = JSON.parse(readFileSync(join(root, 'config', `params.${slug}.json`), 'utf8')).onchain;
    const onChain = await rpc.view(c.Pactora, 'get_params');
    if (JSON.stringify(onChain) !== JSON.stringify(params)) return `get_params on chain differs from config/params.${slug}.json onchain`;
  } catch (e) {
    return `reading ${c.Pactora} on chain: ${String(e.message ?? e).slice(0, 160)}`;
  }
  return null;
}

/** Why the COUNCIL_* / ARBITER_ACCOUNT_ID values in the env would trip create-dao or deploy, or null. */
async function councilProblem({slug, env, rpc, rpcUp, deployerId}) {
  const ids = deployerId ? [deployerId] : [];
  for (const i of [1, 2]) {
    const idName = `COUNCIL_${i}_ACCOUNT_ID`;
    const keyName = `COUNCIL_${i}_SECRET_KEY`;
    const id = env[idName];
    const key = env[keyName];
    if (id) {
      if (!isAccountId(id)) return `${idName} "${id}" is not a valid NEAR account id`;
      if (slug === 'testnet' && !id.endsWith('.testnet')) return `${idName} ${id} is not a .testnet account`;
    }
    if (key) {
      const kp = keyProblem(key);
      if (kp) return `${keyName} ${kp}`;
    }
    const effective = id || (deployerId ? `council-${i}.${deployerId}` : null);
    if (effective) {
      if (ids.includes(effective)) return `${idName} ${effective}: the council must be three distinct accounts (deployer, council 1, council 2)`;
      ids.push(effective);
    }
    // On chain only for testnet: create-dao is not used on the sandbox.
    if (slug !== 'testnet' || !rpcUp || !effective) continue;
    const acct = await rpc.account(effective).catch(() => undefined);
    if (acct && !key) return `${effective} exists but ${keyName} is not set — create-dao would stop (set its key in .env)`;
    if (acct && key) {
      const perm = await rpc.accessKey(effective, publicKeyOf(key)).catch(() => null);
      if (perm !== 'FullAccess') return `${keyName} is not a full-access key of ${effective}`;
    }
  }
  const arbiter = env.ARBITER_ACCOUNT_ID;
  if (arbiter) {
    if (!isAccountId(arbiter)) return `ARBITER_ACCOUNT_ID "${arbiter}" is not a valid NEAR account id`;
    if (slug === 'testnet' && !arbiter.endsWith(`.${FACTORY_TESTNET}`)) return `ARBITER_ACCOUNT_ID ${arbiter} is not a DAO of ${FACTORY_TESTNET} (create-dao.mjs makes one)`;
    if (rpcUp && !(await rpc.account(arbiter).catch(() => null))) return `ARBITER_ACCOUNT_ID ${arbiter} does not exist on ${slug}`;
  }
  return null;
}
