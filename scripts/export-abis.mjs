#!/usr/bin/env node
/**
 * Export ABIs + config into `export/`, the payload consumers install as
 * `@pactora/contracts`.
 *
 * ABIs are GENERATED from the Foundry artifacts, never hand-copied. A changed
 * function signature then becomes a compile error in the backend rather than a
 * runtime revert during a demo.
 *
 *   node scripts/export-abis.mjs
 */

import {readFileSync, writeFileSync, mkdirSync, existsSync, readdirSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'export');

/** Contracts a consumer legitimately needs. Test helpers stay out. */
const EXPORTED = [
  'TaskEscrow',
  'StakeVault',
  'AgentAccount',
  'AgentAccountFactory',
  'ClientReputationWriter',
  'SettlementValidator',
  'AgentDelegate',
  'MockUSDC',
  'MockIdentityRegistry',
  'MockReputationRegistry',
];

if (!existsSync(join(ROOT, 'out'))) {
  console.error('no out/ directory — run `forge build` first');
  process.exit(1);
}

mkdirSync(join(OUT, 'abis'), {recursive: true});
mkdirSync(join(OUT, 'deployments'), {recursive: true});

/**
 * Foundry keys artifacts by SOURCE FILE, not contract name, so several
 * contracts can share a directory (MockERC8004.sol holds two). Search rather
 * than assume `<name>.sol/<name>.json`.
 */
function findArtifact(name) {
  const direct = join(ROOT, 'out', `${name}.sol`, `${name}.json`);
  if (existsSync(direct)) return direct;
  for (const dir of readdirSync(join(ROOT, 'out'))) {
    const candidate = join(ROOT, 'out', dir, `${name}.json`);
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

const abis = {};
for (const name of EXPORTED) {
  const artifact = findArtifact(name);
  if (!artifact) {
    console.error(`missing artifact for ${name} — run \`forge build\``);
    process.exit(1);
  }
  const {abi} = JSON.parse(readFileSync(artifact, 'utf8'));
  abis[name] = abi;
  writeFileSync(join(OUT, 'abis', `${name}.json`), JSON.stringify(abi, null, 2) + '\n');
}

// Config travels with the ABIs so a consumer has one dependency, not three.
writeFileSync(join(OUT, 'networks.json'), readFileSync(join(ROOT, 'config/networks.json')));

const deployed = [];
for (const file of readdirSync(join(ROOT, 'deployments')).filter((f) => f.endsWith('.json'))) {
  writeFileSync(join(OUT, 'deployments', file), readFileSync(join(ROOT, 'deployments', file)));
  deployed.push(file.replace('.json', ''));
}

const names = Object.keys(abis);

writeFileSync(
  join(OUT, 'index.js'),
  `${names.map((n) => `import ${n}Abi from './abis/${n}.json' with {type: 'json'};`).join('\n')}
import networks from './networks.json' with {type: 'json'};

export {${names.map((n) => `${n}Abi`).join(', ')}};
export {networks};

export const deployedChainIds = ${JSON.stringify(deployed.map(Number))};
`,
);

writeFileSync(
  join(OUT, 'index.d.ts'),
  `import type {Abi} from 'viem';
${names.map((n) => `export declare const ${n}Abi: Abi;`).join('\n')}
export declare const networks: Record<string, unknown>;
export declare const deployedChainIds: number[];
`,
);

writeFileSync(
  join(OUT, 'package.json'),
  JSON.stringify(
    {
      name: '@pactora/contracts',
      version: JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8').toString()).version ?? '0.1.0',
      type: 'module',
      main: './index.js',
      types: './index.d.ts',
      exports: {'.': {types: './index.d.ts', default: './index.js'}, './abis/*': './abis/*'},
      files: ['index.js', 'index.d.ts', 'abis', 'networks.json', 'deployments'],
    },
    null,
    2,
  ) + '\n',
);

console.log(`exported ${names.length} ABIs to export/`);
for (const n of names) console.log(`  ${n.padEnd(24)} ${abis[n].length} entries`);
console.log(`deployments: ${deployed.join(', ') || 'none'}`);
