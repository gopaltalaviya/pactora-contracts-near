// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentDelegate} from "../src/AgentDelegate.sol";
import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @title DeployDelegate — AgentDelegate alone (v3)
/// @notice For when only the delegate changed: it is stateless and bound to
///         nothing, so a new one replaces the old in the record and EOAs
///         re-delegate to it. Then
///         `node scripts/record-addon.mjs DeployDelegate.s.sol 10143`.
///         TESTNET only: on mainnet it belongs in the full deploy.
///
///   forge script script/DeployDelegate.s.sol --rpc-url monad_testnet --broadcast \
///       --private-key $DEPLOYER_PRIVATE_KEY
contract DeployDelegate is Script {
    function run() external returns (address delegate) {
        require(block.chainid == 10_143, "DeployDelegate: testnet only");
        vm.startBroadcast();
        delegate = address(new AgentDelegate());
        vm.stopBroadcast();
        console2.log("AgentDelegate :", delegate);
    }
}
