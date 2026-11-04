#!/usr/bin/env bash
# Distribute testnet MON from DEPLOYER to the other three wallets.
#
# The faucet gates on bot detection and social linking, so rather than fight
# it we fund once and spread it. 1 MON is ample for testnet gas.
#
# Your key never leaves your machine: cast reads it from the environment here,
# exactly as `make deploy-testnet` does.
#
#   DEPLOYER_PRIVATE_KEY=0x... ./scripts/fund-wallets.sh
set -euo pipefail

RPC="${RPC_URL_10143:-https://testnet-rpc.monad.xyz}"
: "${DEPLOYER_PRIVATE_KEY:?set DEPLOYER_PRIVATE_KEY (it stays on this machine)}"

FUNDER=0x5c03DB6fb41c0F42777dDE051d07EAb9F54fCc54
AGENT_A=0xfB95885d3A72A82836f3fCAC720Bf80A4155F6c2
AGENT_B=0x2Ff72eE6B8de27dD8F2D9FbFf7102EfbaD8D37D6

# DEPLOYER keeps the largest share: it pays for every contract deployment.
send() {
  echo "  -> $2  $1 MON"
  cast send "$2" --value "${1}ether" --rpc-url "$RPC" \
    --private-key "$DEPLOYER_PRIVATE_KEY" >/dev/null
}

echo "Funding from DEPLOYER on Monad testnet:"
send 0.15 "$FUNDER"
send 0.15 "$AGENT_A"
send 0.15 "$AGENT_B"

echo
echo "Balances now:"
for a in 0xd5b812EFb94124E737c3520739d04539a8441137 "$FUNDER" "$AGENT_A" "$AGENT_B"; do
  printf "  %s  %s MON\n" "$a" "$(cast balance "$a" --rpc-url "$RPC" --ether)"
done
