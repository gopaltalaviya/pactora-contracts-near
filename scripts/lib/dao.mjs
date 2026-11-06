/**
 * The arbiter as a Sputnik DAO (v2): a 2-of-3 council rules disputes.
 *
 * Pactora's `resolve_dispute` checks only `predecessor == arbiter`. With the
 * arbiter set to a DAO, a ruling is a DAO proposal — a FunctionCall to
 * Pactora — that executes only when the council has voted it through. No one
 * member, the deployer included, can rule alone.
 *
 * Testnet: the DAO is created by the official factory `sputnikv2.testnet`
 * (checked 2026-11-05: 5,403 DAOs, `get_daos` works). Sandbox: the factory
 * does not exist, so the same DAO code is deployed directly — the wasm read
 * from a testnet DAO, accepted only if its sha256 (base58) equals the
 * factory's `get_default_code_hash`, so the sandbox runs the exact code the
 * factory deploys.
 */
import {createHash} from 'node:crypto';
import {actions, nearToYocto} from 'near-api-js';

export const TGAS = 1_000_000_000_000n;
export const FACTORY_TESTNET = 'sputnikv2.testnet';
/** `get_default_code_hash` of sputnikv2.testnet, read 2026-11-06. */
export const DAO_CODE_HASH = 'AA4nA45vauiac3jdVwJTzSdUP3oj2r1fDaPAb2mHdgcH';
/** Bond a proposal locks until it is decided (returned when it passes). */
export const PROPOSAL_BOND = nearToYocto('0.1');

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
export function base58(buf) {
  let n = BigInt(`0x${Buffer.from(buf).toString('hex') || '0'}`);
  let out = '';
  while (n > 0n) {
    out = B58[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of buf) {
    if (b !== 0) break;
    out = `1${out}`;
  }
  return out;
}
export const codeHash = (wasm) => base58(createHash('sha256').update(wasm).digest());

/**
 * One role, `council`: three accounts, more than half must approve
 * (Sputnik's threshold [1, 2] with 3 members = 2 votes). Everyone else may
 * do nothing — not even add a proposal.
 */
export function councilPolicy(council) {
  if (council.length !== 3 || new Set(council).size !== 3) throw new Error('the council is exactly three distinct accounts');
  const vote = {weight_kind: 'RoleWeight', quorum: '0', threshold: [1, 2]};
  return {
    roles: [
      {
        name: 'council',
        kind: {Group: council},
        permissions: ['*:AddProposal', '*:VoteApprove', '*:VoteReject', '*:VoteRemove', '*:Finalize'],
        vote_policy: {},
      },
    ],
    default_vote_policy: vote,
    proposal_bond: PROPOSAL_BOND.toString(),
    proposal_period: String(7n * 24n * 3600n * 1_000_000_000n),
    bounty_bond: PROPOSAL_BOND.toString(),
    bounty_forgiveness_period: String(24n * 3600n * 1_000_000_000n),
  };
}

export const daoArgs = (council, purpose) => ({
  config: {name: 'pactora-arbiter', purpose, metadata: ''},
  policy: councilPolicy(council),
});

/** Sandbox: deploy the verified DAO code to `daoId` and initialise it, in one transaction. */
export async function deployDaoSandbox(root, daoId, wasm, council) {
  const h = codeHash(wasm);
  if (h !== DAO_CODE_HASH) throw new Error(`DAO wasm hash ${h} is not the factory's ${DAO_CODE_HASH}`);
  const out = await root.signAndSendTransaction({
    receiverId: daoId,
    actions: [
      actions.createAccount(),
      actions.transfer(nearToYocto('10')),
      actions.deployContract(wasm),
      actions.functionCall('new', daoArgs(council, 'Rules Pactora disputes (sandbox)'), 100n * TGAS, 0n),
    ],
    waitUntil: 'FINAL',
  });
  return out.transaction.hash;
}

/** Testnet: ask the factory for `<name>.sputnikv2.testnet`. */
export async function createDaoTestnet(root, name, council) {
  const args = Buffer.from(JSON.stringify(daoArgs(council, 'Rules Pactora disputes (testnet)'))).toString('base64');
  const out = await root.signAndSendTransaction({
    receiverId: FACTORY_TESTNET,
    actions: [actions.functionCall('create', {name, args}, 150n * TGAS, nearToYocto('6'))],
    waitUntil: 'FINAL',
  });
  return {daoId: `${name}.${FACTORY_TESTNET}`, hash: out.transaction.hash};
}

/** The proposal kind for "Pactora.resolve_dispute(job_id, for_worker)". */
export function rulingKind(pactora, jobId, forWorker) {
  return {
    FunctionCall: {
      receiver_id: pactora,
      actions: [
        {
          method_name: 'resolve_dispute',
          args: Buffer.from(JSON.stringify({job_id: Number(jobId), for_worker: forWorker})).toString('base64'),
          deposit: '0',
          gas: String(50n * TGAS),
        },
      ],
    },
  };
}

function failure(out) {
  if (typeof out.status === 'object' && 'Failure' in out.status) return JSON.stringify(out.status.Failure);
  const r = out.receipts_outcome.find((x) => typeof x.outcome.status === 'object' && 'Failure' in x.outcome.status);
  return r ? JSON.stringify(r.outcome.status.Failure) : null;
}

/** A council member proposes the ruling; returns {id, hash}. */
export async function proposeRuling(member, daoId, pactora, jobId, forWorker, description) {
  const kind = rulingKind(pactora, jobId, forWorker);
  const out = await member.signAndSendTransaction({
    receiverId: daoId,
    actions: [actions.functionCall('add_proposal', {proposal: {description, kind}}, 100n * TGAS, PROPOSAL_BOND)],
    waitUntil: 'FINAL',
    throwOnFailure: false,
  });
  const f = failure(out);
  if (f) throw new Error(`add_proposal: ${f}`);
  const id = JSON.parse(Buffer.from(out.status.SuccessValue, 'base64').toString('utf8'));
  return {id, kind, hash: out.transaction.hash};
}

/** One council vote. The vote that reaches 2 of 3 executes the ruling in the same transaction. */
export async function vote(member, daoId, id, kind, action = 'VoteApprove') {
  const out = await member.signAndSendTransaction({
    receiverId: daoId,
    // `proposal` is checked against the stored kind (Sputnik v2.x), so a vote
    // cannot be replayed onto a different proposal under the same id.
    actions: [actions.functionCall('act_proposal', {id, action, proposal: kind}, 300n * TGAS, 0n)],
    waitUntil: 'FINAL',
    throwOnFailure: false,
  });
  const f = failure(out);
  if (f) throw new Error(`act_proposal ${action}: ${f}`);
  return out.transaction.hash;
}
