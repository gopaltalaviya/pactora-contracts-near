# agentx-contracts

Smart contracts for **Pactora (formerly AGENTX)** — proof-of-payment
reputation and escrow for [ERC-8004](https://eips.ethereum.org/EIPS/eip-8004)
agents, settled on Monad. *Pactora — where AI agents make deals they can't
break.*

The rename touches prose and the tooling package names (`@pactora/contracts`,
2026-11-01) only. The on-chain tags (`"agentx"`, `"agentx-settlement"`),
the storage slot `agentx.storage.AgentAccount`, the repo name and everything
under `src/`, `script/` and `test/` keep the old name: deployed source stays
byte-identical so explorer verification keeps matching, and reviews already on
chain carry the old tag. See agentx-docs `docs/23-launch-and-rebrand-plan.md`.

ERC-8004 gives agents identity and a place to record reputation. It does not
give them money, and `giveFeedback()` is free for anyone to call. Pactora makes
that feedback cost something: **only a settled on-chain payment can write it.**

```solidity
// ERC-8004 requires you to name whose feedback you count —
// getSummary reverts on an empty client list.
getSummary(agentId, [PACTORA_ESCROW], "agentx", "settled")
```

## Why this is needed, with numbers

An empirical study of the live ERC-8004 ecosystem — *["Can Trustless Agents Be
Trusted?"](https://arxiv.org/html/2606.26028)*, Xiong et al., 2026 — measured
what reputation is worth when anyone can write it for free:

| | |
|---|---|
| Median cost to move an agent's score | **$0.0027** on Base |
| Reviewers flagged as Sybil | **90.6%** on Base |
| Feedback carrying neither payment proof nor task linkage | **98.7–100%** |
| Rated agents with no valid feedback left, once Sybil reviews are removed | **77.9–86.8%** |

The paper's first recommendation is to "require evidence-backed interactions
(payment proofs or validated tasks)". That is what these contracts do: only
`TaskEscrow` can write Pactora feedback, and only after a payment settles.

## Relationship to ERC-8183

[ERC-8183 Agentic Commerce](https://eips.ethereum.org/EIPS/eip-8183) (Draft,
Feb 2026) standardises the job-escrow layer — Open → Funded → Submitted →
Completed/Rejected/Expired, with a permissionless refund after expiry. That is
substantially `TaskEscrow`'s shape, arrived at independently, and it is worth
saying so plainly.

What ERC-8183 deliberately leaves unspecified is where this project's work is:
**agent spending caps, staking, dispute resolution, and a micro-payment fast
path**. Its own security note says a malicious evaluator "can complete/reject
arbitrarily" and recommends a reputation system for high-value jobs — and the
study above is the evidence that a reputation system which is not
payment-backed does not help. These two are complementary, not competing.

A function-by-function mapping of `TaskEscrow` against the ERC-8183 draft,
including where the two differ (silence after delivery pays the worker here
and refunds the client there) and why conformance would need a new kernel
rather than an adapter, is in the backend repository at
`agentx-docs/docs/12-erc8183-mapping.md`.

## What is here

| Contract | Role |
|---|---|
| `TaskEscrow` | Job state machine, settlement, and the sole writer of Pactora feedback |
| `StakeVault` | Bonds keyed by ERC-8004 `agentId` — the custody ERC-8004 lacks |
| `AgentAccount` | Spending caps enforced **on-chain**, not on a server |
| `AgentAccountFactory` | ERC-1167 clones, deterministic via CREATE2; only the owner may create theirs |
| `MockUSDC` | 6-decimal test token, testnet only |

Identity and reputation are **not** reimplemented — the ERC-8004 registries are
used directly. On Monad mainnet they are already deployed; on testnet they are
absent, so the reference implementation is deployed alongside the protocol.

## Live on Monad testnet — v2

Deployed on chain **10143** on 2026-09-30 (source not yet verified on the
explorer — that needs an `EXPLORER_API_KEY`). v2 fixes every finding of the
2026-09-30 internal audit ([SECURITY.md](SECURITY.md)); the v1 contracts it
replaces stay on chain as history, and because `getSummary` filters by the
escrow address, **reputation history starts again at v2's escrow**.

Every agent in the backend demo acts through an `AgentAccount`: the
orchestrator pays through one capped per task and per day; each worker acts
through one with zero caps that may call only `TaskEscrow.acceptJob` and
`TaskEscrow.submitResult`. The judges' hosted orchestrator, behind the live
site at https://agentx-interface-iota.vercel.app, pays through one capped at
0.1 MockUSDC per task and 5 MockUSDC per day.

| Contract | Address |
|---|---|
| `MockUSDC` | [`0x35Ca89EA58b292BF8D66eFEC2c086501a3802980`](https://testnet.monadexplorer.com/address/0x35Ca89EA58b292BF8D66eFEC2c086501a3802980) |
| `MockIdentityRegistry` | [`0xeD34Ffc39Ee69c780586b85d930368Bc29B30ef1`](https://testnet.monadexplorer.com/address/0xeD34Ffc39Ee69c780586b85d930368Bc29B30ef1) |
| `MockReputationRegistry` | [`0xCdB4be378D4B276184923E7ad8C11007fd0247bd`](https://testnet.monadexplorer.com/address/0xCdB4be378D4B276184923E7ad8C11007fd0247bd) |
| `StakeVault` | [`0x9E4Da70C473cCA89cbA871A89B4c604B278BF0c7`](https://testnet.monadexplorer.com/address/0x9E4Da70C473cCA89cbA871A89B4c604B278BF0c7) |
| `TaskEscrow` | [`0x4feED0338761817417Fd1dDdFC8331D16AEB370D`](https://testnet.monadexplorer.com/address/0x4feED0338761817417Fd1dDdFC8331D16AEB370D) |
| `AgentAccountFactory` | [`0xcCa4464071B71beE85D8390073492048721849ca`](https://testnet.monadexplorer.com/address/0xcCa4464071B71beE85D8390073492048721849ca) |

Checklist for a (re)deployment: [DEPLOYMENT.md](DEPLOYMENT.md).

## Quickstart

```bash
git clone --recurse-submodules git@github.com:gopaltalaviya/agentx-contracts.git
cd agentx-contracts
make build && make test
```

## Configuration

Every fact is declared in exactly one place:

| Domain | Source |
|---|---|
| Chain facts | `config/networks.json` |
| Protocol parameters | `config/params.<chainId>.json` |
| Deployed addresses | `deployments/<chainId>.json` — **generated** |
| Secrets | environment variables only |

No contract address is ever an env var, and no chain is ever hardcoded. The
same parameter files are read by the deploy script, the backend, and the UI, so
on-chain enforcement and off-chain validation cannot disagree.

## Commands

```bash
make build            # compile
make test             # unit + fuzz + invariant
make coverage         # with branch-coverage floors
make lint             # forge fmt, solhint, Slither
make snapshot         # gas snapshot (deterministic tests)
make verify-erc8004   # probe the ERC-8004 registries on every network
make deploy-testnet   # Monad testnet (10143)
make deploy-mainnet   # Monad mainnet (143) — requires typing the chain id
```

## Networks

| | Chain ID | ERC-8004 registries |
|---|---|---|
| Monad testnet | 10143 | deployed by us (absent upstream) |
| Monad mainnet | 143 | `0x8004A169…a432` / `0x8004BAa1…9b63` |

Verified on-chain 2026-09-22. The mainnet registries are ERC-1967 proxies;
`make verify-erc8004` fails if the implementation moves.

## Security properties

Stated as properties rather than adjectives, because each one is tested:

- **Every non-terminal state has a deadline and a permissionless exit** —
  including `DISPUTED` (`expireDispute`). No party, and not the arbiter, can
  strand funds by going offline, and `pause()` stops only new jobs.
- **A payment that cannot be delivered is held, not lost.** A cleared wallet,
  a blacklisted address or a reverting identity registry credits `owed`,
  which the agent's owner claims with `claimOwed`.
- **Spending caps are enforced here, not on a server.** A fully compromised
  signer still cannot exceed `perTaskCap`, `dailyCap`, or call anything outside
  the per-(target, selector) allowlist, and no allowance-granting selector can
  ever be allowed. Session keys are capped at 24 hours on grant. The daily
  window is fixed, not rolling.
- **Transfer-in is measured as a balance delta.** A fee-on-transfer token
  reverts with `TokenDeliveredLess` instead of quietly leaving the escrow
  insolvent.
- **Feedback cannot break settlement.** The ERC-8004 registry is external and
  may revert or be upgraded badly; `giveFeedback` runs in a try/catch and
  emits `FeedbackFailed` rather than trapping a payment.
- **A review costs a real fee.** Two agents with the same owner cannot hire
  each other (`SameOwner`), nothing below `minJobAmount` is accepted, and every
  job pays at least `minFee`. A cost floor, not sybil-proofing: two owners with
  two bonds can still pay each other — at a fee per review.
- **Admin changes are slow.** The admin role moves in two steps behind a delay.

414 tests: unit, fuzz, adversarial, and invariant suites for the escrow, the
vault and the account. Branch coverage 100% on `StakeVault`, `AgentAccount` and
the factory, 96.5% on `TaskEscrow` (the rest is inside inline assembly) —
enforced in CI, with solhint and Slither.

`via_ir = true` is required, not stylistic: ERC-8004's eleven-argument
`NewFeedback` event overflows the stack without it.

## What is not solved

Stated here rather than left for a reader to find:

- **Result quality is not cryptographically verified.** Schema conformance and
  hash integrity are; truth is not.
- **Disputes are centralised** to one arbiter key — the deployer's, on testnet —
  which can act only on disputed jobs and only until the dispute times out.
- **`slash()` exists and is role-gated, but nothing calls it automatically.**
- **No cross-chain settlement.**
- **Not externally audited.** 414 tests (unit, fuzz, three invariant suites,
  adversarial), Slither and solhint in CI, and an internal audit
  ([SECURITY.md](SECURITY.md)) — but no third-party audit. One reason this is
  testnet-only.
- **Source not yet verified on the explorer** — it needs an `EXPLORER_API_KEY`
  (see [DEPLOYMENT.md](DEPLOYMENT.md)).
- **Scoring is a damped mean, not a trimmed mean.** A coordinated ring of
  colluding agents could still move a score — but every review in it costs a
  full settled payment, which is the entire point. The study above recommends
  trimmed-mean aggregation as well; that is not implemented.

## License

MIT
