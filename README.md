# pactora-contracts-near

NEAR smart contracts for **Pactora**: escrow, bonds, reputation and spend
caps for AI agents that hire each other. *Pactora — where AI agents make deals
they can't break.*

> **Built for the NEAR Hackathon (Nov 2026).** Pactora started as AgentX on
> Monad (EVM, Solidity). The tag `pre-hackathon` is that baseline, imported
> unchanged; everything NEAR — this Rust code — was written during the
> hackathon window. Compare `pre-hackathon...hackathon-submission` to see it.
> (One exception to "unchanged": the baseline lacks the EVM repo's two
> third-party submodule pointers, `lib/forge-std` and
> `lib/openzeppelin-contracts`, which `git archive` does not export. The
> Solidity sources themselves are complete.)
> Sibling repos: `pactora-backend-near`, `pactora-interface-near`,
> `pactora-docs-near`.

## What is here

| Path | What |
|---|---|
| `contracts/pactora` | The protocol, one contract: agent registry, escrow state machine, worker and dispute bonds, rolling 24 h spend caps, reputation, roles, NEP-297 events |
| `contracts/mock-usdc` | Testnet-only NEP-141 token with 6 decimals; only its owner mints |
| `contracts/pactora/src/tests.rs` | Unit tests and a property test of the protocol invariants |
| `contracts/pactora/tests/sandbox.rs` | Integration tests on a local NEAR node against the compiled wasm |

## How it works, in one paragraph

An owner registers an agent and names its wallet — a NEAR account holding a
**function-call key scoped to this contract**. The wallet accepts. The agent is
funded with `ft_transfer_call` and, to be hired, stakes. A client agent hires a
worker (`create_job`); the amount moves from its balance into escrow, under
its per-task and rolling 24-hour caps. The worker accepts (its bond is
locked), submits a result hash, and is paid when the client approves — or
automatically if the client stays silent. A dispute costs the client a bond;
the arbiter (a Sputnik DAO) rules, or the dispute expires. Every open state has
an exit anyone may call. Because a function-call key cannot attach a deposit,
and every method that moves money out needs one yocto, **a leaked agent key can
spend only within its caps and can never withdraw.**

## Build and test

Linux (or WSL) with the toolchain in `rust-toolchain.toml`:

```sh
cargo test --workspace --lib                     # unit + property tests
cargo test -p pactora --test sandbox             # local NEAR node, real wasm
(cd contracts/pactora && cargo near build non-reproducible-wasm)
```

CI (`.github/workflows/ci.yml`) runs all of the above on every push and
uploads the built wasm with its sha256.

## Licence

MIT — see `LICENSE`.
