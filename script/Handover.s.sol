// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../src/StakeVault.sol";
import {TaskEscrow} from "../src/TaskEscrow.sol";
import {ISafe} from "./safe/ISafe.sol";
import {Script} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";
import {console2} from "forge-std/console2.sol";

/// @title Handover — move every role from the deployer to the owner's Safe (v3)
/// @notice The deploy leaves the deployer as admin, configurer and pauser of
///         TaskEscrow and StakeVault (the arbiter is already the arbiter Safe:
///         Deploy.s.sol requires one off testnet). This starts the hand-over:
///
///         1. `Handover` (the deployer sends): the admin Safe gets CONFIGURER
///            on both contracts and PAUSER on the escrow, the deployer loses
///            them, and the two-step default-admin transfer to the Safe
///            begins. It completes no sooner than `defaultAdminDelay()`
///            (1 h on testnet, 2 days on 143).
///         2. `HandoverAccept` (after the delay): the Safe calls
///            `acceptDefaultAdminTransfer()` on both. On testnet it is signed
///            with two of the Safe's `.env` keys and sent; elsewhere it only
///            prints the two calls for the Safe's signers.
///
///         Afterwards the deployer holds no role on any contract —
///         `agentx-backend/scripts/check-launch.mjs` checks exactly that.
///
///   ADMIN_SAFE=0x… forge script script/Handover.s.sol:Handover \
///       --rpc-url monad_testnet --broadcast --private-key $DEPLOYER_PRIVATE_KEY
contract Handover is Script {
    function run() external {
        address adminSafe = vm.envAddress("ADMIN_SAFE");
        (TaskEscrow escrow, StakeVault vault) = Deployed.read(vm);
        address deployer = msg.sender;

        vm.startBroadcast();
        _begin(escrow, vault, deployer, adminSafe);
        vm.stopBroadcast();

        (, uint48 schedule) = escrow.pendingDefaultAdmin();
        console2.log("Admin transfer to", adminSafe);
        console2.log("  acceptable from (unix):", schedule);
        console2.log("Then: forge script script/Handover.s.sol:HandoverAccept");
    }

    /// @dev Grants before revokes, so a role never has no holder mid-way.
    function _begin(TaskEscrow escrow, StakeVault vault, address deployer, address adminSafe)
        internal
    {
        require(adminSafe.code.length > 0, "ADMIN_SAFE: must be a contract (a Safe)");
        require(adminSafe != deployer, "ADMIN_SAFE: is the deployer");
        require(
            !escrow.hasRole(escrow.ARBITER_ROLE(), deployer),
            "deployer is still the arbiter: deploy with ARBITER_ADDRESS set to the arbiter Safe"
        );

        escrow.grantRole(escrow.CONFIGURER_ROLE(), adminSafe);
        escrow.grantRole(escrow.PAUSER_ROLE(), adminSafe);
        vault.grantRole(vault.CONFIGURER_ROLE(), adminSafe);

        escrow.revokeRole(escrow.CONFIGURER_ROLE(), deployer);
        escrow.revokeRole(escrow.PAUSER_ROLE(), deployer);
        vault.revokeRole(vault.CONFIGURER_ROLE(), deployer);

        escrow.beginDefaultAdminTransfer(adminSafe);
        vault.beginDefaultAdminTransfer(adminSafe);
    }
}

/// @notice Step 2: the Safe accepts the admin role on both contracts.
///
///   ADMIN_SAFE=0x… forge script script/Handover.s.sol:HandoverAccept \
///       --rpc-url monad_testnet --broadcast --private-key $DEPLOYER_PRIVATE_KEY
contract HandoverAccept is Script {
    function run() external {
        ISafe safe = ISafe(vm.envAddress("ADMIN_SAFE"));
        (TaskEscrow escrow, StakeVault vault) = Deployed.read(vm);
        bytes memory accept = abi.encodeCall(escrow.acceptDefaultAdminTransfer, ());

        if (block.chainid != 10_143) {
            console2.log("Not testnet: nothing is sent. Have the Safe's signers execute:");
            console2.log("  to   :", address(escrow));
            console2.log("  to   :", address(vault));
            console2.log("  data :");
            console2.logBytes(accept);
            return;
        }

        // Testnet only: the Safe's owners are the .env keys (ArbiterSafe.s.sol).
        uint256[] memory keys = new uint256[](2);
        keys[0] = vm.envUint("DEPLOYER_PRIVATE_KEY");
        keys[1] = vm.envUint("AGENT_A_PRIVATE_KEY");

        vm.startBroadcast();
        _exec(safe, address(escrow), accept, keys);
        _exec(safe, address(vault), accept, keys);
        vm.stopBroadcast();

        require(escrow.defaultAdmin() == address(safe), "escrow admin is not the Safe");
        require(vault.defaultAdmin() == address(safe), "vault admin is not the Safe");
        console2.log("Admin of TaskEscrow and StakeVault:", address(safe));
    }

    /// @dev One Safe transaction, signed by `keys` in ascending owner order.
    function _exec(ISafe safe, address to, bytes memory data, uint256[] memory keys) internal {
        bytes32 hash =
            safe.getTransactionHash(to, 0, data, 0, 0, 0, 0, address(0), address(0), safe.nonce());
        if (vm.addr(keys[0]) > vm.addr(keys[1])) (keys[0], keys[1]) = (keys[1], keys[0]);
        bytes memory sigs;
        for (uint256 i; i < keys.length; i++) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(keys[i], hash);
            sigs = abi.encodePacked(sigs, r, s, v);
        }
        require(
            safe.execTransaction(to, 0, data, 0, 0, 0, 0, address(0), payable(0), sigs),
            "Safe transaction failed"
        );
    }
}

/// @dev The escrow and vault of `deployments/<chainId>.json`.
library Deployed {
    function read(Vm vm_) internal view returns (TaskEscrow escrow, StakeVault vault) {
        string memory json =
            vm_.readFile(string.concat("deployments/", vm_.toString(block.chainid), ".json"));
        escrow = TaskEscrow(vm_.parseJsonAddress(json, ".contracts.TaskEscrow"));
        vault = StakeVault(vm_.parseJsonAddress(json, ".contracts.StakeVault"));
    }
}
