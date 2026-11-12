/**
 * Pure helpers for scripts/deploy-credentials.mjs, kept apart so they can be
 * unit-tested without a node (scripts/credentials.test.mjs).
 */

/**
 * deployments/<network>.json with the credentials contract added: sets
 * `contracts.Credentials` and `wasm.credentials` and nothing else. Every other
 * field, and the order of the existing keys, is kept as it was.
 */
export function mergeCredentials(deployment, {accountId, wasmSha256}) {
  if (!deployment || typeof deployment !== 'object' || Array.isArray(deployment)) throw new Error('deployment must be a JSON object');
  if (!deployment.contracts || typeof deployment.contracts !== 'object') throw new Error('deployment has no contracts object');
  if (typeof accountId !== 'string' || !accountId) throw new Error('accountId is required');
  if (!/^[0-9a-f]{64}$/.test(wasmSha256 ?? '')) throw new Error('wasmSha256 must be 64 hex characters');
  const out = structuredClone(deployment);
  out.contracts = {...out.contracts, Credentials: accountId};
  out.wasm = {...(out.wasm ?? {}), credentials: wasmSha256};
  return out;
}
