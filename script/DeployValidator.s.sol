// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {SettlementValidator} from "../src/SettlementValidator.sol";
import {IValidationRegistry} from "../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../src/interfaces/ITaskEscrow.sol";
import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @title DeployValidator — add SettlementValidator beside a live escrow
/// @notice Deploy.s.sol creates the validator with every new deployment. This
///         adds one beside the v3 escrow already live on testnet, read from
///         deployments/<chainId>.json, without redeploying anything else.
///         Then `node scripts/record-addon.mjs DeployValidator.s.sol 10143` adds it to that
///         record. TESTNET only: on mainnet it belongs in the full deploy.
///
///   forge script script/DeployValidator.s.sol --rpc-url monad_testnet --broadcast \
///       --private-key $DEPLOYER_PRIVATE_KEY
contract DeployValidator is Script {
    function run() external returns (address validator) {
        require(block.chainid == 10_143, "DeployValidator: testnet only");
        string memory record =
            vm.readFile(string.concat("deployments/", vm.toString(block.chainid), ".json"));
        address escrow = vm.parseJsonAddress(record, ".contracts.TaskEscrow");
        address registry = vm.parseJsonAddress(
            vm.readFile("config/networks.json"),
            string.concat(".", vm.toString(block.chainid), ".erc8004.validationRegistry")
        );
        require(escrow.code.length > 0, "TaskEscrow in the record has no code");
        require(registry.code.length > 0, "validationRegistry has no code");

        vm.startBroadcast();
        validator =
            address(new SettlementValidator(ITaskEscrow(escrow), IValidationRegistry(registry)));
        vm.stopBroadcast();

        console2.log("TaskEscrow          :", escrow);
        console2.log("ValidationRegistry  :", registry);
        console2.log("SettlementValidator :", validator);
    }
}
