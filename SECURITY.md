# Security

## Status: internally audited, not externally audited, testnet only

These contracts have **not** had an external audit. They are deployed on Monad
testnet (chain 10143) with test tokens and must not hold real funds until one
has been done (agentx-docs `docs/21-launch-runbook.md` §0). Mainnet
deployment requires typing the chain id (`make deploy-mainnet`), refuses to
deploy the mock token or the reference ERC-8004 registries, and refuses any
admin, arbiter, fee or slash recipient that is unset or is not a contract
(`test/script/DeployGuards.t.sol`).

What has been done instead, and what it does and does not establish:

| | |
|---|---|
| Internal audit, 2026-10-26 (v3) | agentx-docs `docs/22-security-audit.md`: 4 High, 5 Medium, 8 Low, 11 Informational across the contracts and the backend. Every High and Medium fixed, each with a test that fails on the code before the fix (`V3UnwrittenReview`, `V3RoleVisibility`, `V3AuditLows`, the session-manager tests in `AgentAccount.t.sol`); the rest fixed, accepted or documented there. The testnet contracts were redeployed after the fixes. |
| Internal audit, 2026-09-30 (history: v1 → v2) | 3 High, 8 Medium, 12 Low. All fixed in v2; each fix has a test written against v1 first and seen to fail there (`test/unit/V2Findings.t.sol`). |
| Unit, fuzz, adversarial tests | 10,000 fuzz runs per property in CI (`FOUNDRY_PROFILE=ci`) |
| Invariants | escrow solvency (`balance == lockedTotal + owedTotal + disputeBondTotal`), every dispute has a deadline, no job is created while paused, the vault holds exactly what it records, no rolling 24 hours exceeds an account's daily cap, a stolen session key cannot grant an allowance or change policy — 512 runs × 64 calls in CI |
| Branch coverage floors, enforced in CI | 100% on `AgentAccount`, `AgentAccountCore`, `AgentAccountFactory`, `AgentDelegate`, `ClientReputationWriter`, `SettlementValidator`; `TaskEscrow` ≥ 95% and `StakeVault` ≥ 96%, because forge cannot instrument the inline assembly in their transfer helpers, which unit tests drive (`scripts/check-coverage.mjs`) |
| Static analysis | Slither (fails on medium) and solhint in CI; the remaining low/informational results are triaged inline with their reasons |

None of this is a substitute for an external audit. It establishes that the
stated properties hold under the scenarios we thought of; an audit is for the
ones we did not. What is left for it is listed in docs/22 §5.

## Threat model

The full model is `agentx-docs/docs/04-how-it-works.md` §9. The on-chain
parts, as properties:

- **Every non-terminal job state has a deadline and a permissionless exit** —
  including `DISPUTED` (`expireDispute`, since v2). No party, and not the
  arbiter, can strand funds by going offline. `pause()` stops new jobs only.
- **A payment that cannot be delivered is held, not lost.** A cleared wallet, a
  token-blacklisted address or a reverting identity registry credits `owed`;
  the agent's owner claims it with `claimOwed`.
- **Reputation costs a real fee.** Hires between two agents with the same owner
  revert (`SameOwner`); nothing below `minJobAmount` is accepted; every job pays
  at least `minFee`. This is a cost floor, not sybil-proofing: two owners with
  two bonds can still pay each other, at a fee per review.
- **A compromised session key is bounded by the account, on chain:** per-task
  and daily caps, a per-(target, selector) allowlist, no allowance-granting
  selector ever, a lifetime of at most 24 hours, and the owner's `revoke` and
  `sweep`. Since v3 the daily cap is **rolling**: no 24-hour period ever sees
  more than `dailyCap` (v2's fixed window let up to 2× the cap be spent across
  a window boundary).
- **Admin changes are slow.** The default admin role transfers in two steps
  behind a delay (`AccessControlDefaultAdminRules`); an account's owner
  transfers in two steps. On testnet the admin, arbiter, pauser and fee roles
  are held by a 2-of-3 Safe and the deployer holds none
  (agentx-docs `docs/19-v3.md`, `deployments/10143.json`).
- **A pauser stops new jobs and nothing else** — it cannot resume or
  configure, so a leaked pauser key costs downtime, never funds. Every exit
  stays open while paused.

Known limits, stated rather than left to be found:

- Disputes are ruled by the arbiter — a 2-of-3 Safe on testnet, not one key.
  The mainnet Safe's signers are the owner's to appoint (docs/21). A dispute
  nobody rules on expires in the worker's favour after `disputeTimeoutSeconds`.
- Sybil resistance is a cost floor, not a proof (docs/22 L-1, L-2): two owners
  with two bonds can still pay each other, at a fee per review.
- `slash` exists but nothing calls it: nobody holds `SLASHER_ROLE`, and the
  launch check requires it to stay empty (docs/22 F3).
- The ERC-8004 identity registry is upgradeable, so it is a trust root
  (docs/22 I5); its implementation is pinned and checked by `verify-erc8004`.
- Scoring is a damped mean, not a trimmed mean.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting
on this repository ("Security" tab → "Report a vulnerability"), with the
contract, the function, and a reproduction if you have one. (The owner still
has to switch private reporting on in the repository settings — docs/21 §0.) Testnet deployments
hold no real value, so there is no bounty; credit is given in the fix commit.
