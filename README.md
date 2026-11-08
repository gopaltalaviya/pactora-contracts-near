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
| `wasm/` | The deployed bytes: both contracts built in the pinned cargo-near image, sha256 in `wasm/SHA256SUMS`. `scripts/deploy.mjs` deploys these by default and refuses them if a hash differs |

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
cargo test --workspace --lib -- --test-threads=1  # unit + property tests (one thread: near-sdk's mocked runtime can crash in parallel)
cargo test -p pactora --test sandbox             # local NEAR node, real wasm
(cd contracts/pactora && cargo near build non-reproducible-wasm)
```

CI (`.github/workflows/ci.yml`) runs all of the above on every push and
uploads the built wasm with its sha256.

Only the cargo-near build is valid: a plain `cargo build` wasm traps every
refusal as `unreachable` on a real node. Without cargo-near installed, build
in the pinned image named in each `Cargo.toml`:

```sh
docker run --rm -u "$(id -u):$(id -g)" -e CARGO_HOME=/tmp/cargo -v "$PWD:/host" -w /host/contracts/pactora \
  sourcescan/cargo-near:0.22.0-rust-1.99.1@sha256:c9655eaf7436b90c87ea8b25739f91063032c5c30ba24cfeafe72c142943b5bb \
  cargo near build non-reproducible-wasm --locked --no-wasmopt
```

## Run it live on a local NEAR node

Do this, see that (Docker; `npm ci` first):

```sh
docker build -t pactora-near-sandbox ../pactora-backend-near/deploy/near-sandbox
docker run -d --name pactora-sbx -p 127.0.0.1:3030:3030 -v "$PWD/.sbx:/data" pactora-near-sandbox
export SANDBOX_KEY_FILE=$PWD/.sbx/validator_key.json
node scripts/deploy.mjs --network sandbox          # → "sandbox deployment matches deployments/sandbox.json"
node scripts/verify-near.mjs --network sandbox     # → "verify-near: 23 checks passed"
node scripts/create-dao.mjs --fetch-wasm dao.wasm  # the Sputnik DAO code testnet's factory deploys, hash-checked
SPUTNIK_DAO_WASM=dao.wasm node scripts/verify-near.mjs --network sandbox --dao   # → "25 checks passed"
```

The run also shows a person (one full-access account that is its own owner and
wallet) registering, staking, being hired and paid, and `SameOwner` refusing
it against its own agents: no contract change needed.

`--dao` makes a 2-of-3 Sputnik DAO the arbiter and shows an outsider refused,
one vote not ruling, and two votes refunding the client.

**Trust, stated plainly.** The DAO decides rulings only while the owner
lets it: Pactora's owner can call `set_arbiter` (and redeploy the contract,
through the full-access key on its account). On testnet the owner is the
deployer, a single key. The 2-of-3 guarantee is therefore "no one council
member rules through the DAO alone", not "no one key can decide a dispute".
Closing that gap is a hand-over: ownership to the DAO (`propose_owner` /
`accept_owner` through a DAO proposal) and the deployer's key removed from
the contract account — the step the Monad build took with its Safe.

## Testnet

```sh
cp .env.example .env    # set DEPLOYER_ACCOUNT_ID and DEPLOYER_SECRET_KEY
node --env-file=.env scripts/create-dao.mjs --network testnet   # council + DAO; ids and keys go to .env only
node --env-file=.env scripts/deploy.mjs --network testnet       # writes deployments/testnet.json
node --env-file=.env scripts/verify-near.mjs --network testnet  # every flow, the DAO ruling included
node --env-file=.env scripts/sweep-testnet.mjs                  # only after a run that stopped half-way
```

The deployer is **your own account**, and no script asks you to fund an account
whose key you do not hold:
- `deploy` gives each contract account the deployer's own key.
- `create-dao` writes the two council keys into your `.env`, or uses your own
  council accounts if you set `COUNCIL_1_*` and `COUNCIL_2_*` first.
- `verify-near` saves each test account's key to `.keys/testnet/` (gitignored)
  before it creates the account. At the end it deletes the full-access ones
  back into the deployer, which returns their NEAR. It does the same at the
  start of the next run, for a run that stopped half-way.

`scripts/sweep-check.mjs` checks that path live on the sandbox.

**Budget: about 25 testnet NEAR for one full pass**, more than one faucet
request, so fund the deployer before starting:

| Step | NEAR | Where it goes |
|---|---|---|
| `create-dao` | ~8 | 6 to the Sputnik factory for the DAO's storage; 1 each to two council accounts |
| `deploy` | ~9 | 3 to the mUSDC account, 6 to the Pactora account (contract storage) |
| `verify-near`, each run | ~7 while it runs, ~1 spent | Three fresh owners at 2 each, deleted back into the deployer at the end. Their agent wallets at 0.2 each stay where they are: a function-call key cannot move NEAR, by design, though their keys are saved. Registration and token-storage deposits go to your contract accounts |

Mainnet is refused by every script.

## Licence

MIT — see `LICENSE`.
