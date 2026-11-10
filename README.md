# pactora-contracts-near

NEAR smart contracts for **Pactora**: escrow, bonds, reputation and spend
caps for AI agents that hire each other. *Pactora — where AI agents make deals
they can't break.*

## Built for NEAR, in the hackathon window

Every item below was built from 2026-11-05 on, in [this history](https://github.com/gopaltalaviya/pactora-contracts-near/commits/human-work):

- **`contracts/pactora`, version 1.1.0**: agent registry, escrow state machine, worker and dispute
  bonds, rolling 24 h spend caps, reputation written only by a settled job, NEP-297 events; 88 tests,
  including model-based property tests and adversarial runs on a local NEAR node.
- **A 2-of-3 Sputnik DAO as the arbiter** (`scripts/create-dao.mjs`), ruling disputes on chain.
- **Function-call keys**: an agent's wallet signs only Pactora calls, so a leaked key cannot withdraw.
- **Reproducible wasm** (NEP-330, pinned cargo-near image) with sha256 in `wasm/SHA256SUMS`;
  `deploy.mjs` refuses bytes whose hash differs.
- **`verify-near`**: 26 live checks on a running chain, 28 with the DAO; `preflight-testnet` before a
  testnet run.

> **Disclosure (rules Q-002, Q-003).** The escrow-and-reputation idea was prototyped before the
> hackathon. That prototype is this repository's first commit, tagged `pre-hackathon` (2026-11-04);
> it is the baseline, not the submission. Everything after it is the NEAR work, dated in the
> history: [`pre-hackathon...hackathon-submission`](https://github.com/gopaltalaviya/pactora-contracts-near/compare/pre-hackathon...hackathon-submission)
> (the tag is added at submission; until then [`pre-hackathon...human-work`](https://github.com/gopaltalaviya/pactora-contracts-near/compare/pre-hackathon...human-work)).
>
> Sibling repos, same layout: `pactora-backend-near`, `pactora-interface-near`, `pactora-docs-near`.
> Libraries used are listed at the end of this file (Q-005); licence is MIT, `LICENSE` (Q-014).

## What is here

| Path | What |
|---|---|
| `contracts/pactora` | The protocol, one contract: agent registry, escrow state machine, worker and dispute bonds, rolling 24 h spend caps, reputation, roles, NEP-297 events |
| `contracts/mock-usdc` | Testnet-only NEP-141 token with 6 decimals; only its owner mints |
| `contracts/pactora/src/tests.rs` | Unit tests and a property test of the protocol invariants; `mod audit` holds the tests written for the 2026-11-09 audit fixes |
| `contracts/pactora/src/fuzz.rs` | Model-based property tests: random call sequences from every kind of caller, an oracle that predicts each call's refusal and the whole post-state, invariants after every step, and a drain at the end (`PROPTEST_CASES` sets the depth) |
| `contracts/pactora/tests/sandbox.rs` | Integration tests on a local NEAR node against the compiled wasm |
| `contracts/pactora/tests/adversarial.rs` | Adversarial tests on the local node: races in one block, gas sweeps, storage spam, a real Sputnik DAO arbiter, an upgrade + `migrate` over live state, every event as the indexer reads it |
| `wasm/` | The deployed bytes: both contracts built in the pinned cargo-near image, sha256 in `wasm/SHA256SUMS`. `scripts/deploy.mjs` deploys these by default and refuses them if a hash differs. `wasm/previous/` is the release before, for the upgrade test |

## How it works, in one paragraph

An owner registers an agent and names its wallet — a NEAR account holding a
**function-call key scoped to this contract**. The wallet accepts. The agent is
funded with `ft_transfer_call` and, to be hired, stakes. A client agent hires a
worker (`create_job`); the amount moves from its balance into escrow, under
its per-task and rolling 24-hour caps. The worker accepts (its bond is
locked), submits a result hash, and is paid when the client approves — or
automatically if the client stays silent. A dispute costs the client a bond
(taken from its balance, outside its caps, since it comes back unless the
dispute is ruled against); the arbiter (a Sputnik DAO) rules — it may do so
until the dispute is actually expired, which anyone may do only after
`dispute_timeout` plus a `dispute_grace`. An expired dispute pays the worker
the job and half the client's bond, for the wait; the other half returns to
the client. Every open state has an exit anyone may call. Because a function-call key cannot attach a deposit,
and every method that moves money out needs one yocto, **a leaked agent key can
spend only within its caps and can never withdraw.**

## Where it sits next to NEAR Agent Market

[NEAR Agent Market](https://docs.market.near.ai) runs the same lifecycle —
the marketplace holds the payment, acceptance releases it, a dispute
deposit, a resolver, a 0–100 reputation — **off-chain and custodial**: an
operator holds the funds and the records. Pactora is that settlement and
reputation layer **on-chain and trustless**: escrow, bonds, caps and the
ruling live in this contract, nobody takes custody, there is no operator
who can move funds or edit a score, and the reputation (`worker_record`,
`client_record`, `verdict_of`) is state any app or agent can read and build
on. The two complement each other rather than compete: a Pactora worker is
an ordinary NEAR account and can be listed on Agent Market, settling its
Agent Market jobs there and its agent-to-agent jobs here, with one
reputation that follows it.

## Build and test

Linux (or WSL) with the toolchain in `rust-toolchain.toml`:

```sh
cargo test --workspace --lib -- --test-threads=1  # unit, property and fuzz tests (one thread: near-sdk's mocked runtime can crash in parallel)
PROPTEST_CASES=2000 cargo test -p pactora --lib fuzz -- --test-threads=1   # a deeper fuzz run (default 256 cases)
PACTORA_WASM=$PWD/wasm/pactora.wasm MOCK_USDC_WASM=$PWD/wasm/mock_usdc.wasm \n  cargo test -p pactora --test sandbox           # local NEAR node, the committed wasm (unset: built with cargo-near)
PACTORA_WASM=$PWD/wasm/pactora.wasm MOCK_USDC_WASM=$PWD/wasm/mock_usdc.wasm \
OLD_PACTORA_WASM=$PWD/wasm/previous/pactora.wasm SPUTNIK_DAO_WASM=$PWD/dao.wasm \
  cargo test -p pactora --test adversarial       # races, gas, a real DAO, upgrade + migrate (the two optional wasms enable those tests)
```

The local node (`near-sandbox` 2.14) needs glibc 2.38+: Ubuntu 24.04 or
Debian trixie; on an older WSL run the sandbox and adversarial tests in a
`rust:1.99.1-trixie` container with the repo mounted. Native Windows
`cargo test` fails to build `near-workspaces` (path length); use WSL.

### Building the wasm

Only a cargo-near build is valid: a plain `cargo build` wasm traps every
refusal as `unreachable` on a real node. Both contracts build with
`--no-wasmopt` (each `Cargo.toml` says why: the image's wasm-opt post-step
segfaults on them; the wasm is larger, not different).

**The committed bytes** (`wasm/pactora.wasm`, `wasm/mock_usdc.wasm`, hashes
in `wasm/SHA256SUMS`) were built with the exact command below: the pinned
cargo-near image named in each `Cargo.toml`, run directly, with
`non-reproducible-wasm --locked --no-wasmopt` inside it. Do this, then
`sha256sum target/near/pactora/pactora.wasm` — it prints the line in
`wasm/SHA256SUMS`:

```sh
docker run --rm -u "$(id -u):$(id -g)" -e CARGO_HOME=/tmp/cargo -v "$PWD:/host" -w /host/contracts/pactora \
  sourcescan/cargo-near:0.22.0-rust-1.99.1@sha256:c9655eaf7436b90c87ea8b25739f91063032c5c30ba24cfeafe72c142943b5bb \
  cargo near build non-reproducible-wasm --locked --no-wasmopt
```

The same command with `-w /host/contracts/mock-usdc` builds the token.
`scripts/deploy.mjs` deploys the committed bytes by default and refuses them
if a hash differs, so a judge needs no Rust toolchain to deploy.

**CI builds differently**, and its hashes are not the committed ones. CI
runs `cargo near build reproducible-wasm` in each contract directory, which
pulls the same pinned image and runs the same `container_build_command`
from `Cargo.toml` inside it, but also fills the NEP-330
`contract_source_metadata.build_info` (image, command, git snapshot of the
commit being built). The committed wasm has `build_info: null` — it is the
direct-image build above, and that is how you can tell the two apart (`cat`
the wasm and look for `"build_info"`). Because the snapshot names the
commit, a `reproducible-wasm` hash can never be checked into the commit it
describes; the committed `SHA256SUMS` is therefore the direct build, and
CI's `wasm.sha256` is the reproducible build of that same source. We have
not diffed the two wasms byte for byte; the expectation that they differ
only in that metadata string comes from the build setup, not from a
measurement.

With cargo-near 0.22.0 installed on the host (Docker still required), the
CI form is `(cd contracts/pactora && cargo near build reproducible-wasm)`;
it needs a clean git tree. `cargo near build non-reproducible-wasm` on the
host without Docker also gives a valid wasm for local testing, with a hash
of its own.

### CI

`.github/workflows/ci.yml` runs on **pushes to `master`, on every pull
request, and on manual dispatch** (`workflow_dispatch`). It does **not** run
on pushes to `human-work`; to run it on this branch, open a pull request or
dispatch it by hand. Five jobs: `cargo fmt --check`, `clippy -D warnings`
and the unit + property tests; the reproducible wasm build (uploaded as the
`wasm` artefact with `wasm.sha256` and the ABI); the sandbox and adversarial
suites against that CI-built wasm, `wasm/previous/pactora.wasm` and a
hash-checked Sputnik DAO wasm fetched from testnet; `deploy.mjs` +
`verify-near.mjs` (26 checks, 28 with `--dao`) on a fresh near-sandbox; and
`scripts/check-no-secrets.mjs` over the tracked files.

## Run it live on a local NEAR node

Do this, see that (Docker; `npm ci` first; clone with `-b human-work`, the
submission branch). A POSIX shell (Linux, macOS, WSL); on Git Bash for
Windows prefix the `docker run` with `MSYS_NO_PATHCONV=1` and use
`$(pwd -W)` for `$PWD`, or the volume mounts the wrong folder. Port 3031
keeps clear of the backend demo's sandbox on 3030:

```sh
docker build -t pactora-near-sandbox ../pactora-backend-near/deploy/near-sandbox
docker run -d --name pactora-sbx -p 127.0.0.1:3031:3030 -v "$PWD/.sbx:/data" pactora-near-sandbox
export SANDBOX_KEY_FILE=$PWD/.sbx/validator_key.json SANDBOX_RPC=http://127.0.0.1:3031
node scripts/deploy.mjs --network sandbox          # → "sandbox deployment matches deployments/sandbox.json"
node scripts/verify-near.mjs --network sandbox     # → "verify-near: 26 checks passed"
node scripts/create-dao.mjs --fetch-wasm dao.wasm  # the Sputnik DAO code testnet's factory deploys, hash-checked
SPUTNIK_DAO_WASM=dao.wasm node scripts/verify-near.mjs --network sandbox --dao   # → "28 checks passed"
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
the contract account.

## Testnet

```sh
cp .env.example .env    # set DEPLOYER_ACCOUNT_ID and DEPLOYER_SECRET_KEY
node --env-file=.env scripts/preflight-testnet.mjs              # 0. read-only go/no-go; spends nothing
node --env-file=.env scripts/create-dao.mjs --network testnet   # council + DAO; ids and keys go to .env only
node --env-file=.env scripts/deploy.mjs --network testnet       # writes deployments/testnet.json
node --env-file=.env scripts/verify-near.mjs --network testnet  # every flow, the DAO ruling included
node --env-file=.env scripts/sweep-testnet.mjs                  # only after a run that stopped half-way
```

Step 0 sends no transaction: it checks the network, the deployer's key (full
access on the account), the balance against the budget below, the wasm hashes,
the params and any `deployments/testnet.json` or council values already there,
then prints `GO` with the commands still to run, or `NO-GO` and exits non-zero
(`npm run test:preflight` unit-tests each refusal).

The deployer is **your own account**, and no script asks you to fund an account
whose key you do not hold:
- `deploy` gives each contract account the deployer's own key, then deploys
  and calls `new` signed as that account: `new` is `#[private]` on both
  contracts, so nobody else can initialise freshly deployed code.
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

## Upgrading a deployed contract

The state blob carries a `version` (view `version`). To move a 1.0.0
deployment to this code, the contract account deploys the new wasm and
calls `migrate` **in the same batch** (both are private to that account);
`migrate` reads the 1.0.0 layout, adds `dispute_grace` (24 h) and refuses
to run twice. Then `set_params` if the grace should differ. The adversarial
test `upgrading_from_the_old_wasm_keeps_every_job_settleable` does exactly
this over live jobs in every state, against `wasm/previous/pactora.wasm`.

## Methods

Every method the contract exports: the 56 `pub fn` in
`contracts/pactora/src/lib.rs` (check: `grep -c "pub fn"
contracts/pactora/src/lib.rs` prints 56) plus the NEP-141 receiver
`ft_on_transfer`, 57 in all. "Who may call" and "Deposit" are read from the
code: `assert_one_yocto()`, `#[payable]`, `#[private]`, and the
`only_owner` / `owned_agent` (the agent's owner) / `only_wallet_of` (the
agent's accepted wallet, agent not frozen) / arbiter checks. A method
without `#[payable]` refuses any attached deposit — so an agent wallet,
which holds a function-call key and cannot attach one, can call exactly the
"none" rows. "Tgas left" is gas remaining in the call, not gas attached;
near-cli's default 30 Tgas is too little for the sending methods, give them
about 35 or more.

**Setup and roles**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `new(owner, token, arbiter, pauser, fee_recipient, params)` | the contract account only (`#[init] #[private]`), once | none | Initialises roles, token and parameters. `deploy.mjs` deploys and calls it in one batch signed as the contract account |
| `migrate()` | the contract account only (`#[private] #[init(ignore_state)]`) | none | Reads the 1.0.0 state layout and writes the 1.1.0 one (`version`, `dispute_grace` = 24 h); refuses to run twice |
| `propose_owner(new_owner)` | owner | 1 yocto | Step one of the two-step ownership transfer |
| `accept_owner()` | the pending owner | 1 yocto | Step two: the proposed account becomes owner |
| `set_arbiter(arbiter)` | owner | 1 yocto | Names the account (the Sputnik DAO) that rules disputes |
| `set_pauser(pauser)` | owner | 1 yocto | Names the account that may pause |
| `set_fee_recipient(fee_recipient)` | owner | 1 yocto | Names where `claim_fees` sends protocol fees |
| `set_params(params)` | owner | 1 yocto | Replaces the parameters (validated). New jobs copy them at creation; `dispute_grace` is read at expiry, so it applies to open disputes |
| `pause()` | pauser or owner | none | Stops `create_job` and `direct_pay` only; every exit stays open |
| `unpause()` | owner | 1 yocto | Lifts the pause |

**Agent registry**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `register_agent(wallet, card_uri, per_task_cap?, daily_cap?)` | anyone; the caller becomes the agent's owner | storage: NEAR ≥ the registration's storage cost, the rest is credited as the agent's storage balance | Creates the agent with a *pending* wallet, caps (defaults from params), an empty purse, stake and records; returns the `AgentId` |
| `accept_wallet(agent_id)` | the proposed wallet account | none | The wallet agrees to act for the agent. One wallet per agent, one agent per wallet |
| `propose_wallet(agent_id, wallet?)` | agent owner | 1 yocto | Proposes a new wallet (key rotation); the old one works until the new one accepts. `null` withdraws the proposal |
| `set_card_uri(agent_id, card_uri)` | agent owner | 1 yocto | Replaces the agent card URI (≤ 512 bytes) |
| `freeze_agent(agent_id)` | agent owner | none | Kill switch: the wallet can do nothing and the agent cannot be hired. Exits anyone may call still end its open jobs |
| `unfreeze_agent(agent_id)` | agent owner | 1 yocto | Lifts a freeze |
| `set_caps(agent_id, per_task_cap, daily_cap)` | agent owner | 1 yocto | Sets the per-task and rolling 24 h spend caps (`per_task ≤ daily`) |
| `storage_deposit(agent_id)` | anyone | payable: any amount of NEAR, all credited | Prepays NEAR for the storage the agent's jobs use; returns the new balance |
| `storage_withdraw(agent_id, amount)` | agent owner | 1 yocto | Sends unused storage NEAR to the owner; a failed transfer is re-credited by `on_storage_sent` |
| `on_storage_sent(agent_id, amount)` | the contract itself (`#[private]` callback) | — | After the NEAR transfer: re-credits the amount if the transfer failed |

**Money out**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `withdraw(agent_id, amount, receiver?)` | agent owner | 1 yocto; ≥ 30 Tgas left | Sends tokens from the agent's balance to `receiver` (default: the owner) by `ft_transfer`; a failed send returns to the balance |
| `claim_fees()` | anyone | none; ≥ 30 Tgas left | Sends the accumulated protocol fees to the fee recipient |
| `on_send(receiver, amount, refund)` | the contract itself (`#[private]` callback) | — | After every `ft_transfer`: clears `in_flight`; on failure puts the amount back in its bucket; emits `unstaked` (success) or `stake_restored` (failure) for stake payouts |
| `recover_surplus(amount, receiver?)` | owner | 1 yocto; ≥ 70 Tgas left | Returns tokens a plain `ft_transfer` left above `get_totals().sum`: reads the token balance, then `on_surplus_balance` sends `min(amount, balance − sum)`. Cannot reach anything the protocol owes |
| `on_surplus_balance(balance, amount, receiver)` | the contract itself (`#[private]` callback) | — | Computes the surplus from the token's answer and starts the send |

**Stake**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `request_unstake(agent_id, amount)` | agent owner | 1 yocto | Moves unlocked stake into a pending withdrawal that matures after `withdraw_delay`; one pending at a time |
| `cancel_unstake(agent_id)` | agent owner | 1 yocto | Returns the pending amount to available stake |
| `complete_unstake(agent_id)` | agent owner | 1 yocto; ≥ 30 Tgas left | Pays matured stake to the owner, never through the agent's balance; `unstaked` is emitted once the transfer lands |

**Escrow**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `create_job(client_id, worker_id, amount, spec_hash, accept_window, work_window)` | the client agent's wallet | none | Moves `amount` from the client's balance into escrow under its caps; checks the worker is hireable and not the same owner; bills the job's storage to the client. Refused while paused |
| `accept_job(job_id)` | the worker agent's wallet | none | Before `accept_deadline`: the worker accepts and locks its bond (counted against its own caps) |
| `submit_result(job_id, result_hash)` | the worker agent's wallet | none | Before `work_deadline`: delivers the result hash and starts the review window |
| `approve(job_id, review_hash?)` | the client agent's wallet | none | Pays the worker (minus the fee), releases its bond, writes both reviews. Works after the review window too, until someone calls `auto_approve` |
| `dispute(job_id, reason_hash?)` | the client agent's wallet | none | Inside the review window: holds the client's dispute bond (balance only, outside the caps) and opens the dispute for `dispute_timeout` |
| `resolve_dispute(job_id, for_worker)` | arbiter | none | Rules for the worker (paid, bond to the worker) or for the client (refund, bond back). Allowed until `expire_dispute` actually runs |
| `cancel(job_id)` | the client agent's wallet | none | Withdraws an offer nobody accepted, at any time |
| `expire_unaccepted(job_id)` | anyone | none | After `accept_deadline`: refunds the client |
| `expire_undelivered(job_id)` | anyone | none | After `work_deadline`: refunds the client and gives it the worker's bond |
| `auto_approve(job_id)` | anyone | none | After `review_deadline` with neither approval nor dispute: pays the worker |
| `expire_dispute(job_id)` | anyone | none | After `dispute_deadline + dispute_grace` with no ruling: pays the worker; half the client's bond goes to the worker, half back to the client |
| `direct_pay(client_id, worker_id, amount, spec_hash)` | the client agent's wallet | none | Instant payment up to `fast_path_max`, no escrow window, settled in one call. Refused while paused |

**Tokens in**

| Method | Who may call | Deposit | What it does |
|---|---|---|---|
| `ft_on_transfer(sender_id, amount, msg)` (NEP-141 receiver) | the token contract only, as the result of anyone's `ft_transfer_call` to the token | none | `msg` `{"Fund":{"agent_id":N}}` credits the agent's balance, `{"Stake":{"agent_id":N}}` its stake. A bad message panics and the token refunds the sender in full |

**Views** — anyone, no deposit, no gas cost through `view`:

| Method | Returns |
|---|---|
| `version()` | layout version of the stored state (1 for 1.1.0) |
| `get_params()` | the current `Params` |
| `get_roles()` | owner, pending owner, arbiter, pauser, fee recipient, token, paused |
| `get_agent(agent_id)` | the `Agent` record, or null |
| `agent_of_wallet(wallet)` | the `AgentId` an accepted wallet acts for, or null |
| `agent_count()` | number of agents registered |
| `get_job(job_id)` | the `Job`, or null |
| `job_count()` | number of jobs created (escrow and direct-pay) |
| `worker_record(agent_id)` | completed, failed, unresolved, disputes lost, reviews and score sum as a worker |
| `client_record(agent_id)` | paid, silent, disputes upheld / rejected, reviews and score sum as a client |
| `purse_of(agent_id)` | balance, caps, spent today, daily remaining, next release time |
| `stake_of(agent_id)` | available, pending, unlock time, locked, unlocked, hireable |
| `is_hireable(agent_id)` | wallet accepted, not frozen, unlocked stake ≥ `min_stake` |
| `storage_balance_of(agent_id)` | the agent's prepaid storage NEAR (yocto) |
| `get_totals()` | the six buckets (locked, dispute bonds, balances, stakes, fees owed, in flight) and their sum — invariant I1 |
| `verdict_of(job_id)` | 100 settled undisputed, 0 dispute upheld, null not attributable |

`contracts/mock-usdc` adds, over the standard NEP-141/NEP-145/NEP-148
methods: `new(owner)` (`#[init] #[private]`), `mint(account_id, amount)`
(owner only) and the view `owner()`.

## 1.1.0 — the 2026-11-09 audit fixes

The internal review (`pactora-docs-near/docs/27-audit-2026-11.md`) found,
for the contracts, **1 medium, 5 low and 11 informational notes, no critical
or high**: the medium and four lows are fixed below; the fifth low (a leaked
client key can burn the agent's prepaid storage NEAR) is a documented limit
in `SECURITY.md`; of the eleven informational notes five are fixed (the
"Safety" bullet) and six are documented in `SECURITY.md` and docs/27 C8. Each change has
a test that failed on 1.0.0 (`mod audit` in `tests.rs`,
`tests/adversarial.rs`):

- **Ruling-vs-expiry race (medium).** `expire_dispute` was permissionless
  the second the deadline passed, so a worker could expire a dispute in the
  same block as a late ruling. Now it works only after
  `dispute_deadline + dispute_grace` (new parameter, 24 h on testnet), and
  the arbiter may rule until the expiry actually executes. The testnet
  `dispute_timeout` is 7 days, the DAO's proposal period.
- **Dispute bond outside the caps (low).** The bond checked the client's
  per-task and daily caps, so a client that had spent its day on the hire
  could not dispute and the worker was auto-approved. The bond now checks
  the balance only; it is held, not spent.
- **A silent arbiter is no longer a free dispute (low).** On expiry half
  the client's bond goes to the worker for the wait; the other half returns.
  `dispute_expired` carries `bond_to_worker` and `bond_to_client`.
- **`unstaked` was logged before the payout landed (low).** It is now
  emitted in `on_send` when the transfer succeeds; `stake_restored` when it
  fails and the stake goes back to `available`.
- **Tokens sent with a plain `ft_transfer` were stuck (low).** Owner-only,
  one-yocto `recover_surplus(amount, receiver)` returns tokens held above
  `get_totals().sum`, and nothing else (two steps: the token's
  `ft_balance_of`, then a private callback sends `min(amount, balance −
  sum)` through the usual `in_flight` / `on_send` path).
- **Safety (informational, fixed).** `new` is private on both contracts; a
  zero accept or work window is `InvalidWindow`, not `WindowTooLong`; the
  state carries a `version` and `migrate` moves 1.0.0 state forward; the
  fuzz suite gained exact-boundary cases (`fuzz_boundaries_exact`); CI runs
  the adversarial suite with the previous wasm and the fetched Sputnik
  wasm.

## Libraries used

Everything third-party this repo builds, tests or runs with (hackathon
rule Q-005). Exact versions are the ones in `Cargo.lock`, `Cargo.toml`,
`package-lock.json` and `.github/workflows/ci.yml`.

| Library | Version | Used for |
|---|---|---|
| [near-sdk](https://crates.io/crates/near-sdk) | 5.30.0 | the contract: `#[near]` macros, `LookupMap`, `env`, promises, NEP-297 events, Borsh / JSON |
| [near-contract-standards](https://crates.io/crates/near-contract-standards) | 5.30.0 | `FungibleTokenReceiver` (`ft_on_transfer`) and `ext_ft_core` in Pactora; the whole NEP-141/145/148 token in `mock-usdc` |
| Rust toolchain | 1.99.1 (`rust-toolchain.toml`), target `wasm32-unknown-unknown` | compiling both contracts |
| [cargo-near](https://github.com/near/cargo-near) | 0.22.0; image `sourcescan/cargo-near:0.22.0-rust-1.99.1` | the only valid wasm build (see *Building the wasm*); NEP-330 source metadata; ABI export |
| [near-workspaces](https://crates.io/crates/near-workspaces) | 0.24.0 (feature `unstable`) | `tests/sandbox.rs` and `tests/adversarial.rs`: deploys the wasm to a local node and drives it |
| [near-sandbox](https://crates.io/crates/near-sandbox) (Rust crate) | 0.4.0, pulled in by near-workspaces | downloads and runs the nearcore sandbox binary for those tests |
| [near-sandbox](https://www.npmjs.com/package/near-sandbox) (npm) | 0.3.5 (nearcore sandbox 2.14) | the local node for `scripts/verify-near.mjs`, in CI's `live` job and in `../pactora-backend-near/deploy/near-sandbox` |
| [proptest](https://crates.io/crates/proptest) | 1.12.0 (`"1"` in `Cargo.toml`) | the property test in `tests.rs` and the model-based suite in `fuzz.rs` |
| [tokio](https://crates.io/crates/tokio) | 1.54.1 | async runtime for the sandbox tests |
| [anyhow](https://crates.io/crates/anyhow), [serde_json](https://crates.io/crates/serde_json) | 1.0.104, 1.0.154 | error handling and JSON in the sandbox tests |
| [near-api-js](https://www.npmjs.com/package/near-api-js) | 7.4.0 | the only npm dependency: `scripts/deploy.mjs`, `verify-near.mjs`, `create-dao.mjs`, `sweep-*.mjs`, `check-no-secrets.mjs` (everything else they import is `node:` built-ins) |
| Node.js | 24 (CI) | running the scripts |
| [Sputnik DAO v2](https://github.com/near-daos/sputnik-dao-contract) | the wasm the testnet factory `sputnikv2.testnet` deploys, fetched and checked against the factory's `get_default_code_hash` (pinned in `scripts/lib/dao.mjs`) by `create-dao.mjs --fetch-wasm` | the 2-of-3 arbiter; not compiled here, not committed (`dao.wasm` is gitignored) |
| GitHub Actions: `actions/checkout@v7`, `Swatinem/rust-cache@v2`, `actions/setup-node@v7`, `actions/upload-artifact@v7`, `actions/download-artifact@v8` | as pinned in `ci.yml` | CI |

AI assistance: Claude Code was used to write and review code and docs.

## Licence

MIT — see `LICENSE`.
