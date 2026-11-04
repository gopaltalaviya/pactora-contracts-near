// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {BaseTest} from "../Base.t.sol";

/// @dev v3: a pause-only role. On mainnet the admin is a multisig with a
///      2-day handover delay, slow to gather in an incident; a pauser can stop
///      new jobs at once. It cannot resume them, or change anything else —
///      resuming is the admin's, so a leaked pauser key costs downtime, never
///      funds. Exits stay open while paused (TaskEscrow.t.sol).
contract V3PauserTest is BaseTest {
    address internal pauser = makeAddr("pauser");

    function setUp() public override {
        super.setUp();
        bytes32 role = escrow.PAUSER_ROLE();
        vm.prank(admin);
        escrow.grantRole(role, pauser);
    }

    function test_adminHoldsThePauserRoleFromTheStart() public view {
        assertTrue(escrow.hasRole(escrow.PAUSER_ROLE(), admin));
    }

    function test_pauserCanPause() public {
        vm.prank(pauser);
        escrow.pause();
        assertTrue(escrow.paused());

        vm.prank(clientWallet);
        vm.expectRevert();
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_pauserCannotUnpause() public {
        vm.prank(pauser);
        escrow.pause();
        vm.prank(pauser);
        vm.expectRevert();
        escrow.unpause();
        assertTrue(escrow.paused());
    }

    function test_pauserCannotConfigure() public {
        vm.prank(pauser);
        vm.expectRevert();
        escrow.setParams(_config(), feeRecipient);
    }

    function test_adminStillPausesAndUnpauses() public {
        vm.prank(admin);
        escrow.pause();
        vm.prank(admin);
        escrow.unpause();
        assertFalse(escrow.paused());
    }

    function test_revokedPauserCannotPause() public {
        bytes32 role = escrow.PAUSER_ROLE();
        vm.prank(admin);
        escrow.revokeRole(role, pauser);
        vm.prank(pauser);
        vm.expectRevert();
        escrow.pause();
    }

    function test_jobsInFlightSettleWhileThePauserHasPaused() public {
        uint256 jobId = _submittedJob();
        vm.prank(pauser);
        escrow.pause();
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(escrow.lockedTotal(), 0);
    }
}
