# Security

## Status: testnet only, not audited

These NEAR contracts are new (NEAR Hackathon, November 2026) and have **not**
been audited, internally or externally. They run on NEAR testnet with a mock
token and must not hold real funds. The EVM version they port (AgentX v3)
had an internal audit; its findings that apply to NEAR are carried over, and
a NEAR-specific review is part of the hackathon plan (pactora-docs-near).

## What the design defends against

- **A leaked agent key.** Agent wallets hold a function-call key scoped to
  this contract. Such a key cannot attach a deposit; every method that moves
  money out (`withdraw`, unstaking, storage withdrawal) needs one yocto, so
  only the owner's full-access key can take money out. What the key can spend
  is bounded by per-task and rolling 24-hour caps.
- **Half-done settlements.** Payments between agents are internal credits in
  one synchronous call. Tokens leave only through `withdraw` and
  `claim_fees`; a failed transfer goes back where it came from
  (`on_send`, `#[private]`).
- **Storage drain.** Agents prepay the storage their jobs use, billed when the
  job is created, including what the job may grow by later.
- **Trapped funds.** Pause stops new hires only. Every open state has an exit
  anyone may call.

## Known limits, stated plainly

- **I1 is "at least", not "exactly".** `Totals` accounts for every token the
  contract was given through `ft_transfer_call`. A plain `ft_transfer` to the
  contract raises its token balance without any agent being credited; such
  tokens are not owned by anyone in the protocol and there is no sweep. (The
  same holds for a direct ERC-20 transfer on EVM.) Tests check equality,
  because they fund only through `ft_transfer_call`.
- **A callback that never runs.** `on_send` and `on_storage_sent` are given
  fixed gas and do little work, so they should always complete. If one ever
  failed, a successful transfer would leave `in_flight` overstated (the
  accounting reads more than is held) and a failed one would leave the
  amount stranded in the contract. There is no owner "reconcile" method on
  purpose: it would be a way to move money that the protocol does not need.
- **The dispute bond counts against the client's caps,** as it did on EVM
  (AgentAccount charged it through `execute`). A client that has used its
  whole daily cap cannot dispute until the window rolls; the owner can raise
  the caps with `set_caps`.
- **Reserved job storage is not refunded.** Jobs are history and are never
  deleted, so their storage stays billed.

## Reporting

Please report vulnerabilities privately through GitHub's "Report a
vulnerability" (Security tab) on this repository. Do not open a public issue.
