#!/usr/bin/env node
/**
 * Fail CI when branch coverage of a production contract drops below its floor.
 *
 *   forge coverage --ir-minimum --report lcov && node scripts/check-coverage.mjs
 *
 * Branch coverage, not line coverage: the branches are the guards, and a guard
 * nobody tests is a guard nobody knows works. The floors are set just below
 * what the suite reaches today, so a new untested branch fails the build.
 *
 * TaskEscrow's floor is below 100% only because forge cannot instrument
 * inline assembly: the two `if` blocks inside `_walletOf` and `_tryTransfer`
 * always read as uncovered, though unit tests drive both (V2Findings H3).
 * StakeVault's is below 100% for the same reason since v3: its `_tryTransfer`
 * (used by `seize`) has the same assembly `if`, and V3StakeLocks drives all
 * three arms — a normal token, one that reverts, one that returns nothing.
 */
import {readFileSync, readdirSync, statSync} from 'node:fs';
import {join} from 'node:path';

const FLOORS = {
  'src/TaskEscrow.sol': 95,
  'src/StakeVault.sol': 96,
  'src/AgentAccount.sol': 100,
  'src/AgentAccountFactory.sol': 100,
  'src/ClientReputationWriter.sol': 100,
  'src/SettlementValidator.sol': 100,
  'src/AgentAccountCore.sol': 100,
  'src/AgentDelegate.sol': 100,
};

const lcovPath = process.argv[2] ?? 'lcov.info';

// A report older than the code it describes measures some other code. When
// `forge coverage` fails to compile it leaves the previous lcov.info behind,
// and reading that would pass a build that never ran.
const newest = (dir) =>
  readdirSync(dir, {recursive: true})
    .filter((f) => String(f).endsWith('.sol'))
    .reduce((m, f) => Math.max(m, statSync(join(dir, String(f))).mtimeMs), 0);
const lcovAge = statSync(lcovPath).mtimeMs;
if (lcovAge < Math.max(newest('src'), newest('test'))) {
  console.error(`  ✗ ${lcovPath} is older than the sources — did forge coverage fail? Re-run it.`);
  process.exit(1);
}

const lcov = readFileSync(lcovPath, 'utf8');
const stats = {};
let file = null;
for (const raw of lcov.split('\n')) {
  const line = raw.trim();
  if (line.startsWith('SF:')) file = line.slice(3).replaceAll('\\', '/');
  else if (file && line.startsWith('BRF:')) (stats[file] ??= {}).found = Number(line.slice(4));
  else if (file && line.startsWith('BRH:')) (stats[file] ??= {}).hit = Number(line.slice(4));
}

let failed = 0;
for (const [path, floor] of Object.entries(FLOORS)) {
  const s = stats[path];
  if (!s || !s.found) {
    console.error(`  ✗ ${path}: no branch data — did coverage run?`);
    failed++;
    continue;
  }
  const pct = (100 * s.hit) / s.found;
  const ok = pct + 1e-9 >= floor;
  console.log(`  ${ok ? '✓' : '✗'} ${path.padEnd(28)} ${pct.toFixed(2)}% branches (floor ${floor}%)`);
  if (!ok) failed++;
}
if (failed) {
  console.error(`\n${failed} contract(s) below their branch-coverage floor.`);
  process.exit(1);
}
