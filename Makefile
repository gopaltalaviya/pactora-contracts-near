.PHONY: help install build test test-ci coverage snapshot snapshot-check fmt lint drift export \
        deploy deploy-local deploy-testnet deploy-mainnet dry-run-mainnet handover handover-accept check-launch \n        verify-erc8004 publish clean

# Network alias from foundry.toml [rpc_endpoints]. One deploy target,
# parameterised — not one target per network that drifts apart.
NETWORK ?= monad_testnet

help:
	@echo "Pactora contracts"
	@echo ""
	@echo "  make install          install deps (forge-std, openzeppelin)"
	@echo "  make build            compile"
	@echo "  make test             run tests"
	@echo "  make test-ci          tests with CI profile (10k fuzz runs)"
	@echo "  make coverage         coverage summary"
	@echo "  make snapshot         gas snapshot"
	@echo "  make fmt              format"
	@echo "  make verify-erc8004   probe the ERC-8004 registries on both networks"
	@echo "  make drift            deployed params vs config/params.<chainId>.json"
	@echo "  make export           ABIs + config -> export/ for @pactora/contracts"
	@echo ""
	@echo "  make deploy-local     -> anvil"
	@echo "  make deploy-testnet   -> Monad testnet (10143)"
	@echo "  make dry-run-mainnet  simulate the mainnet deploy on a fork; sends nothing"
	@echo "  make deploy-mainnet   -> Monad mainnet (143), requires confirmation"
	@echo "  make handover         roles -> ADMIN_SAFE; make handover-accept after the delay"
	@echo "  make check-launch     read-only: no deployer roles, Safes, wiring, params"

# The libraries are git submodules pinned to exact commits (forge-std v1.9.4,
# OpenZeppelin v5.0.2). `forge install --no-git` ignored those pins.
install:
	git submodule update --init --recursive

build:
	forge build

test:
	forge test -vvv

test-ci:
	FOUNDRY_PROFILE=ci forge test -vvv

# --ir-minimum is REQUIRED, not a preference.
#
# forge coverage turns viaIR off for accurate line mapping, and this project
# cannot compile without it — ERC-8004's 11-argument NewFeedback event blows
# the stack. Without this flag the command fails with "Stack too deep", which
# is what it had been doing here and in CI.
coverage:
	forge coverage --ir-minimum --report summary --report lcov
	node scripts/check-coverage.mjs lcov.info

# Deterministic tests only: fuzz and invariant gas varies with the seed.
SNAPSHOT_FILTER = --no-match-test "testFuzz|invariant" --no-match-path "test/invariant/*"

snapshot:
	forge snapshot $(SNAPSHOT_FILTER)

snapshot-check:
	forge snapshot --check $(SNAPSHOT_FILTER)

fmt:
	forge fmt

lint:
	forge fmt --check
	npx --yes solhint@5.0.5 "src/**/*.sol"
	slither . --config-file slither.config.json --fail-medium

secrets:
	node scripts/check-no-secrets.mjs

verify-erc8004:
	node scripts/verify-erc8004.mjs

drift:
	node scripts/check-param-drift.mjs $(NETWORK)

export:
	node scripts/export-abis.mjs

# Verification is opt-in (VERIFY=1). Explorer verifiers reject or rate-limit
# often, and a failed verify must not look like a failed deploy — the
# contracts are already on-chain by then. Verify afterwards with `make verify`.
VERIFY_FLAG := $(if $(VERIFY),--verify,)

deploy:
	forge script script/Deploy.s.sol:Deploy --rpc-url $(NETWORK) --broadcast $(VERIFY_FLAG) -vvv
	node scripts/write-deployment.mjs $(NETWORK)

verify:
	forge script script/Deploy.s.sol:Deploy --rpc-url $(NETWORK) --verify --resume

deploy-local:
	$(MAKE) deploy NETWORK=anvil

deploy-testnet:
	$(MAKE) deploy NETWORK=monad_testnet

# A wrong NETWORK is one character away from a real-money mistake.
deploy-mainnet:
	@echo "*** MAINNET DEPLOY (Monad, chain 143) ***"
	@echo "Type the chain id to confirm:"
	@read -r c; [ "$$c" = "143" ] || { echo "aborted"; exit 1; }
	$(MAKE) deploy NETWORK=monad

# The mainnet deploy, simulated against a fork of 143 with the same role
# holders the real one will use. Nothing is broadcast. Needs ARBITER_ADDRESS,
# FEE_RECIPIENT, SLASH_RECIPIENT, PAUSER_ADDRESS (Safes) and DEPLOYER_ADDRESS.
dry-run-mainnet:
	forge script script/Deploy.s.sol:Deploy --fork-url $${RPC_URL_143:-https://rpc.monad.xyz} 		--sender $${DEPLOYER_ADDRESS:?set DEPLOYER_ADDRESS} -vvv

# docs/20 §4: every role from the deployer to ADMIN_SAFE (two steps; the
# second after the chain's adminDelaySeconds). Off testnet the second only
# prints the Safe transactions to sign.
handover:
	forge script script/Handover.s.sol:Handover --rpc-url $(NETWORK) --broadcast -vvv

handover-accept:
	forge script script/Handover.s.sol:HandoverAccept --rpc-url $(NETWORK) --broadcast -vvv

# docs/20 §3–§4 in one read-only command. Needs DEPLOYER_ADDRESS, ADMIN_SAFE,
# ARBITER_ADDRESS and PAUSER_ADDRESS.
check-launch:
	node scripts/check-launch.mjs $(NETWORK) --deployer $${DEPLOYER_ADDRESS:?set DEPLOYER_ADDRESS} 		--admin-safe $${ADMIN_SAFE:?set ADMIN_SAFE}

publish:
	$(MAKE) export
	cd export && npm version patch && npm publish

clean:
	forge clean
	rm -rf export
