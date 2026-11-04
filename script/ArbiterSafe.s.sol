// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ISafe, ISafeProxyFactory, SafeAddresses} from "./safe/ISafe.sol";
import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @title ArbiterSafe — create the multisig that decides disputes (v3)
/// @notice Owner decision (2026-10-25), TESTNET: a 2-of-3 Safe over the
///         DEPLOYER, AGENT_A and AGENT_B keys already in `.env`, so disputes
///         can be exercised end to end. Mainnet needs keys the owner holds
///         itself — see the launch checklist.
///
///         Only the private keys' ADDRESSES are derived here; the broadcast
///         is signed by whoever runs the script. Then deploy with
///         `ARBITER_ADDRESS=<printed address>` and Deploy.s.sol moves the
///         arbiter role to the Safe.
///
///   forge script script/ArbiterSafe.s.sol --rpc-url monad_testnet --broadcast \
///       --private-key $DEPLOYER_PRIVATE_KEY
contract ArbiterSafe is Script {
    function run() external returns (address safe) {
        require(
            block.chainid == 10_143, "ArbiterSafe: testnet only; mainnet signers are the owner's"
        );

        address[] memory owners = new address[](3);
        owners[0] = vm.addr(vm.envUint("DEPLOYER_PRIVATE_KEY"));
        owners[1] = vm.addr(vm.envUint("AGENT_A_PRIVATE_KEY"));
        owners[2] = vm.addr(vm.envUint("AGENT_B_PRIVATE_KEY"));

        bytes memory init = abi.encodeCall(
            ISafe.setup,
            (owners, 2, address(0), "", SafeAddresses.FALLBACK_HANDLER, address(0), 0, payable(0))
        );

        vm.startBroadcast();
        safe = ISafeProxyFactory(SafeAddresses.PROXY_FACTORY)
            .createProxyWithNonce(
                SafeAddresses.SAFE_L2, init, uint256(keccak256("agentx-v3-arbiter"))
            );
        vm.stopBroadcast();

        require(ISafe(safe).getThreshold() == 2, "ArbiterSafe: threshold");
        console2.log("Arbiter Safe (2-of-3):", safe);
    }
}
