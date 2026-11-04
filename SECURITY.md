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

## Reporting

Please report vulnerabilities privately through GitHub's "Report a
vulnerability" (Security tab) on this repository. Do not open a public issue.
