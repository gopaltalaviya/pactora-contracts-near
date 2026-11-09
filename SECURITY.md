# Security

## Status: testnet only, internally reviewed, not externally audited

These NEAR contracts are new (NEAR Hackathon, November 2026) and have **not**
been externally audited. They run on NEAR testnet with a mock token and must
not hold real funds. An internal review on 2026-11-09 (three independent reviewers: code
review, model-based property tests, adversarial tests on a local node; the
tests are now in the repo) found, for the contracts: **1 medium (fixed),
5 low (4 fixed, 1 documented below as the storage-burn limit) and 11
informational notes (5 fixed, 6 documented)**, no critical or high. The
fixes are 1.1.0 and are listed in the README; the full report is
`pactora-docs-near/docs/27-audit-2026-11.md`. Nothing it found could take
or trap money that was not the caller's.

## What the design defends against

- **A leaked agent key.** Agent wallets hold a function-call key scoped to
  this contract. Such a key cannot attach a deposit; every method that moves
  money out (`withdraw`, unstaking, storage withdrawal) needs one yocto, so
  only the owner's full-access key can take money out. What the key can spend
  is bounded by per-task and rolling 24-hour caps.
- **Half-done settlements.** Payments between agents are internal credits in
  one synchronous call. Tokens leave only through `withdraw`, `claim_fees`,
  `complete_unstake` and `recover_surplus`; a failed transfer goes back
  where it came from (`on_send`, `#[private]`), and `unstaked` is emitted
  only once the stake has actually left.
- **A raced ruling.** The arbiter may rule until a dispute is actually
  expired, and nobody may expire it before `dispute_deadline +
  dispute_grace` (24 h on testnet, after a 7-day timeout that matches the
  DAO's proposal period), so a late ruling cannot be beaten to the block by
  the party it would go against.
- **A blocked dispute.** The dispute bond checks only the client's balance,
  not its caps: a client that spent its whole day on the hire can still
  contest the result. The bond is held, not spent, and comes back unless
  the dispute is ruled against.
- **Initialisation and upgrades.** `new` and `migrate` are `#[private]`:
  only the contract account, which alone can deploy code, may call them.
- **Storage drain.** Agents prepay the storage their jobs use, billed when the
  job is created, including what the job may grow by later.
- **Trapped funds.** Pause stops new hires only. Every open state has an exit
  anyone may call.

## Known limits, stated plainly

- **I1 is "at least", not "exactly".** `Totals` accounts for every token the
  contract was given through `ft_transfer_call`. A plain `ft_transfer` to the
  contract raises its token balance without any agent being credited; such
  tokens are not owned by anyone in the protocol. The owner may return them with
  `recover_surplus`, which reads the token balance and sends at most
  `balance − get_totals().sum` — it cannot reach a unit the protocol owes.
  Tests check equality, because they fund only through `ft_transfer_call`.
- **A callback that never runs.** `on_send`, `on_storage_sent` and
  `on_surplus_balance` are given fixed gas and do little work, so they
  should always complete. If `on_send` ever failed, a successful transfer
  would leave `in_flight` overstated (the accounting reads more than is
  held) and a failed one would leave the amount stranded in the contract,
  still counted in `in_flight`, so `recover_surplus` cannot reach it
  either. There is no owner "reconcile" of the totals on purpose: it would
  be a way to move money that the protocol does not need.
- **An unanswered dispute costs the client half its bond.** If the arbiter
  never rules, the expiry pays the worker the job and half the bond for the
  wait it was put through, and returns the other half. A client should
  expect its arbiter to rule; the owner can lengthen `dispute_grace` for
  disputes already open, since the grace is read at expiry.
- **`dispute_grace` is read at expiry, not copied into the job** (unlike
  the windows and bonds), so a parameter change applies to open disputes.
- **A leaked client key can burn the agent's prepaid storage NEAR** with
  create/cancel loops (each cycle costs the same, every token comes back);
  `freeze_agent` ends it. With the testnet default caps a worker cannot
  accept a job above 500 000 units, since its 20 % bond counts toward its
  own per-task cap; `set_caps` raises it. Both are tested as known limits.
- **Reserved job storage is not refunded.** Jobs are history and are never
  deleted, so their storage stays billed.

## Reporting

Please report vulnerabilities privately through GitHub's "Report a
vulnerability" (Security tab) on this repository. Do not open a public issue.
