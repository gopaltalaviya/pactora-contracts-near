// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccountFactory} from "../src/AgentAccountFactory.sol";
import {AgentDelegate} from "../src/AgentDelegate.sol";
import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @title DeployAccounts — the agent wallets, beside a live escrow (v3)
/// @notice AgentAccount now runs on AgentAccountCore (namespaced storage, the
///         same ABI), and AgentDelegate brings the same controls to an EOA by
///         EIP-7702. This deploys a factory whose implementation is the
///         current AgentAccount, and the delegate, for the token in
///         deployments/<chainId>.json — nothing else. Accounts the previous
///         factory created keep their implementation.
///         Then `node scripts/record-addon.mjs DeployAccounts.s.sol 10143`.
///         TESTNET only: on mainnet both belong in the full deploy.
///
///   forge script script/DeployAccounts.s.sol --rpc-url monad_testnet --broadcast \
///       --private-key $DEPLOYER_PRIVATE_KEY
contract DeployAccounts is Script {
    function run() external returns (address factory, address delegate) {
        require(block.chainid == 10_143, "DeployAccounts: testnet only");
        string memory record =
            vm.readFile(string.concat("deployments/", vm.toString(block.chainid), ".json"));
        address token = vm.parseJsonAddress(record, ".contracts.PaymentToken");
        require(token.code.length > 0, "PaymentToken in the record has no code");

        vm.startBroadcast();
        factory = address(new AgentAccountFactory(token));
        delegate = address(new AgentDelegate());
        vm.stopBroadcast();

        console2.log("PaymentToken        :", token);
        console2.log("AgentAccountFactory :", factory);
        console2.log("  implementation    :", AgentAccountFactory(factory).implementation());
        console2.log("AgentDelegate       :", delegate);
    }
}
