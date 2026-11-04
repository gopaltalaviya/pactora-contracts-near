// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {Handover} from "../../script/Handover.s.sol";
import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {Test} from "forge-std/Test.sol";

/// @dev Runs the script's steps as the deployer: the harness IS the deployer.
contract HandoverHarness is Handover {
    function begin(TaskEscrow escrow, StakeVault vault, address safe) external {
        _begin(escrow, vault, address(this), safe);
    }
}

/// @dev v3, docs/20 §3: after the hand-over the deployer holds no role on
///      either contract, and the admin Safe holds every one it needs.
contract HandoverTest is Test {
    uint48 internal constant DELAY = 1 days;
    HandoverHarness internal deployer;
    TaskEscrow internal escrow;
    StakeVault internal vault;
    address internal adminSafe;
    address internal arbiterSafe = makeAddr("arbiter safe");

    function setUp() public {
        deployer = new HandoverHarness();
        adminSafe = address(new SafeStandIn());
        escrow = new TaskEscrow(address(deployer), DELAY);
        vault = new StakeVault(address(deployer), DELAY);
        // As Deploy.s.sol leaves it off testnet: the arbiter is its own Safe.
        vm.startPrank(address(deployer));
        escrow.grantRole(escrow.ARBITER_ROLE(), arbiterSafe);
        escrow.revokeRole(escrow.ARBITER_ROLE(), address(deployer));
        vm.stopPrank();
    }

    function _accept() internal {
        vm.warp(block.timestamp + DELAY + 1);
        vm.startPrank(adminSafe);
        escrow.acceptDefaultAdminTransfer();
        vault.acceptDefaultAdminTransfer();
        vm.stopPrank();
    }

    function test_afterAcceptingTheDeployerHoldsNoRole() public {
        deployer.begin(escrow, vault, adminSafe);
        _accept();

        address d = address(deployer);
        assertFalse(escrow.hasRole(escrow.DEFAULT_ADMIN_ROLE(), d));
        assertFalse(escrow.hasRole(escrow.CONFIGURER_ROLE(), d));
        assertFalse(escrow.hasRole(escrow.ARBITER_ROLE(), d));
        assertFalse(escrow.hasRole(escrow.PAUSER_ROLE(), d));
        assertFalse(vault.hasRole(vault.DEFAULT_ADMIN_ROLE(), d));
        assertFalse(vault.hasRole(vault.CONFIGURER_ROLE(), d));
    }

    function test_afterAcceptingTheSafeHoldsEveryRoleItNeeds() public {
        deployer.begin(escrow, vault, adminSafe);
        _accept();

        assertEq(escrow.defaultAdmin(), adminSafe);
        assertEq(vault.defaultAdmin(), adminSafe);
        assertTrue(escrow.hasRole(escrow.CONFIGURER_ROLE(), adminSafe));
        assertTrue(escrow.hasRole(escrow.PAUSER_ROLE(), adminSafe));
        assertTrue(vault.hasRole(vault.CONFIGURER_ROLE(), adminSafe));
        assertTrue(escrow.hasRole(escrow.ARBITER_ROLE(), arbiterSafe), "arbiter untouched");
    }

    function test_theSafeCannotAcceptBeforeTheDelay() public {
        deployer.begin(escrow, vault, adminSafe);
        vm.prank(adminSafe);
        vm.expectRevert();
        escrow.acceptDefaultAdminTransfer();
    }

    function test_refusesAnAdminWithoutCode() public {
        vm.expectRevert(bytes("ADMIN_SAFE: must be a contract (a Safe)"));
        deployer.begin(escrow, vault, makeAddr("an EOA"));
    }

    function test_refusesWhileTheDeployerIsStillTheArbiter() public {
        bytes32 arbiter = escrow.ARBITER_ROLE();
        vm.prank(address(deployer));
        escrow.grantRole(arbiter, address(deployer));
        vm.expectRevert(
            bytes(
                "deployer is still the arbiter: deploy with ARBITER_ADDRESS set to the arbiter Safe"
            )
        );
        deployer.begin(escrow, vault, adminSafe);
    }
}

contract SafeStandIn {}
