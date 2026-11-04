// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev v3: accepting a job locks part of the worker's bond — a share of the
///      job amount, never below a floor, fixed when the job is created. Not
///      delivering costs the worker that bond, paid to the client with the
///      refund. Every other ending returns it.
contract V3JobBondsTest is BaseTest {
    function _expectedBond(uint128 amount) internal pure returns (uint128 b) {
        b = uint128((uint256(amount) * BOND_BPS) / 10_000);
        if (b < MIN_BOND) b = MIN_BOND;
    }

    function test_theBondIsFixedWhenTheJobIsCreated() public {
        uint256 jobId = _createJob();
        assertEq(escrow.getJob(jobId).workerBond, _expectedBond(PRICE));

        TaskEscrow.Config memory c = _config();
        c.bondBps = 5_000;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);

        vm.prank(workerWallet);
        escrow.acceptJob(jobId);
        assertEq(vault.lockedOf(workerId), _expectedBond(PRICE), "captured, not current");
    }

    function test_acceptingLocksTheBond() public {
        _acceptedJob();
        assertEq(vault.lockedOf(workerId), _expectedBond(PRICE));
    }

    function test_theFloorAppliesToSmallJobs() public {
        vm.prank(clientWallet);
        uint256 jobId = escrow.createJob(
            clientId, workerId, MIN_JOB_AMOUNT, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW
        );
        assertEq(escrow.getJob(jobId).workerBond, MIN_BOND);
    }

    /// @dev Both jobs are created while the worker is hireable; the first
    ///      accept locks the whole bond, so the second cannot be backed.
    function test_aWorkerWithoutEnoughUnlockedStakeCannotAccept() public {
        TaskEscrow.Config memory c = _config();
        c.bondBps = 10_000; // bond = the whole amount
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        vm.startPrank(clientWallet);
        uint256 first = escrow.createJob(
            clientId, workerId, MAX_JOB_AMOUNT, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW
        );
        uint256 second = escrow.createJob(
            clientId, workerId, MIN_JOB_AMOUNT, keccak256("t"), ACCEPT_WINDOW, WORK_WINDOW
        );
        vm.stopPrank();
        // MAX_JOB_AMOUNT (10 USDC) == MIN_STAKE: the first takes it all.
        vm.prank(workerWallet);
        escrow.acceptJob(first);
        vm.prank(workerWallet);
        vm.expectRevert(IStakeVault.InsufficientBond.selector);
        escrow.acceptJob(second);
    }

    /// @dev A fully locked worker is not offered new work at all.
    function test_aFullyLockedWorkerIsNotHireable() public {
        TaskEscrow.Config memory c = _config();
        c.bondBps = 10_000;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        vm.prank(clientWallet);
        uint256 jobId = escrow.createJob(
            clientId, workerId, MAX_JOB_AMOUNT, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW
        );
        vm.prank(workerWallet);
        escrow.acceptJob(jobId);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.NotHireable.selector);
        escrow.createJob(
            clientId, workerId, MIN_JOB_AMOUNT, keccak256("t"), ACCEPT_WINDOW, WORK_WINDOW
        );
    }

    function test_approvalReturnsTheBond() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(vault.lockedOf(workerId), 0);
        assertEq(vault.bondOf(workerId), MIN_STAKE);
    }

    function test_notDeliveringCostsTheBondAndTheClientGetsIt() public {
        uint256 jobId = _acceptedJob();
        uint128 bond = _expectedBond(PRICE);
        uint256 before = token.balanceOf(clientWallet);
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);

        vm.expectEmit(true, true, false, true, address(escrow));
        emit ITaskEscrow.BondSeized(jobId, workerId, bond);
        escrow.expireUndelivered(jobId);

        assertEq(token.balanceOf(clientWallet) - before, PRICE + bond, "refund plus the bond");
        assertEq(vault.bondOf(workerId), MIN_STAKE - bond);
        assertEq(vault.lockedOf(workerId), 0);
    }

    /// @dev The owner's v3 decision: a lost dispute refunds the client but
    ///      does not take the bond — the arbiter judged quality, not absence.
    function test_aLostDisputeReturnsTheBond() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        assertEq(vault.lockedOf(workerId), 0);
        assertEq(vault.bondOf(workerId), MIN_STAKE);
    }

    function test_anExpiredDisputeReturnsTheBond() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);
        escrow.expireDispute(jobId);
        assertEq(vault.lockedOf(workerId), 0);
    }

    function test_autoApprovalReturnsTheBond() public {
        uint256 jobId = _submittedJob();
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        assertEq(vault.lockedOf(workerId), 0);
    }

    /// @dev The escrow pays the bond out of what the vault sent it; a vault on
    ///      a different token would make it pay in the wrong currency.
    function test_aVaultOnAnotherTokenIsRefused() public {
        MockUSDC other = new MockUSDC();
        vm.startPrank(admin);
        StakeVault v = new StakeVault(admin, ADMIN_DELAY);
        v.configure(address(other), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient);
        TaskEscrow e = new TaskEscrow(admin, ADMIN_DELAY);
        vm.expectRevert(TaskEscrow.TokenMismatch.selector);
        e.configure(
            address(token),
            address(identity),
            address(reputation),
            address(v),
            feeRecipient,
            _config()
        );
        vm.stopPrank();
    }

    function test_bondBpsAbove100PercentIsRefused() public {
        TaskEscrow.Config memory c = _config();
        c.bondBps = 10_001;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidBond.selector);
        escrow.setParams(c, feeRecipient);
    }
}
