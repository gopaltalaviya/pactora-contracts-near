# Deployment checklist

Every step is a command or a check with an expected result. Nothing here is
"remember to".

## Before

- [ ] `make install` — submodules at their pinned commits (forge-std v1.9.4,
      OpenZeppelin v5.0.2).
- [ ] `make lint` — `forge fmt --check`, solhint, Slither with no medium or
      higher finding.
- [ ] `FOUNDRY_PROFILE=ci make test` — all green, 10k fuzz runs and 512×64
      invariants.
- [ ] `make coverage` — every contract at or above its branch floor.
- [ ] `node scripts/check-config.mjs` — every network has the same parameter
      keys, with sane values (`minFee < minJobAmount ≤ fastPathMax`, non-zero
      windows, mainnet windows and delays at least a day).
- [ ] `.env` has `DEPLOYER_PRIVATE_KEY` and `RPC_URL_<chainId>`, and optionally
      `FEE_RECIPIENT`, `ARBITER_ADDRESS`, `SLASH_RECIPIENT` (all default to the
      deployer, which is acceptable on testnet only).
- [ ] The deployer holds enough native coin: about 0.5 MON on Monad testnet,
      because Monad charges the full gas limit of each transaction.
- [ ] **Local dry run:** start `anvil`, then
      `forge script script/Deploy.s.sol:Deploy --rpc-url anvil --broadcast --private-key <anvil key 0>`,
      then `node scripts/write-deployment.mjs anvil` and
      `RPC_URL_31337=http://127.0.0.1:8545 node scripts/check-param-drift.mjs anvil` → `no drift`.

## Deploy

- [ ] Testnet: `make deploy-testnet`. Mainnet: `make deploy-mainnet`, which
      asks you to type the chain id and refuses to deploy mocks.
- [ ] `node scripts/write-deployment.mjs <network>` — writes
      `deployments/<chainId>.json` with a real `deployedAt` and `startBlock`.
- [ ] `node scripts/check-param-drift.mjs <network>` → `no drift`.
- [ ] `node scripts/export-abis.mjs` — the backend reads ABIs and addresses
      from `export/`.

## After

- [ ] `EXPLORER_API_KEY` set → `make verify NETWORK=<network>`, so explorer
      links show source, not bytecode.
- [ ] Hand over roles: `beginDefaultAdminTransfer(multisig)` on `TaskEscrow`
      and `StakeVault`, wait `adminDelaySeconds`, `acceptDefaultAdminTransfer`
      from the multisig. Move `ARBITER_ROLE` and `CONFIGURER_ROLE` to the
      multisig and renounce them from the deployer.
- [ ] Backend: `node scripts/sync-chain-facts.mjs` in `agentx-backend` and
      commit `chain/` — the hosted images carry that copy, not this repo, and
      the backend's CI fails until it matches.
- [ ] Backend: reset the indexer cursor for the chain (new `startBlock`), run
      the migrations, and run `scripts/e2e.mjs` and `scripts/verify-indexer.mjs`
      against the new addresses.
- [ ] Record the addresses, the deploy commit and the evidence in the
      backend's `PROGRESS.md`. A new `TaskEscrow` address starts a new
      reputation history: `getSummary` filters by the escrow address.
