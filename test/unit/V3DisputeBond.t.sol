// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev v3: a dispute costs the client a bond, fixed when the job is created.
///      A dispute the arbiter rejects pays the bond to the worker, so a free
///      "dispute everything" stops being free. Owner decision (2026-10-25):
///      when the arbiter never rules, the worker is paid in full (as v2) and
///      the client's bond comes back — the arbiter's silence is not the
///      client's fault.
contract V3DisputeBondTest is BaseTest {
    function _expected(uint128 amount) internal pure returns (uint128 b) {
        b = uint128((uint256(amount) * DISPUTE_BOND_BPS) / 10_000);
        if (b < MIN_DISPUTE_BOND) b = MIN_DISPUTE_BOND;
    }

    function _disputed() internal returns (uint256 jobId) {
        jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
    }

    function test_theDisputeBondIsFixedAtCreation() public {
        uint256 jobId = _createJob();
        assertEq(escrow.getJob(jobId).disputeBond, _expected(PRICE));
        TaskEscrow.Config memory c = _config();
        c.disputeBondBps = 9_000;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        assertEq(escrow.getJob(jobId).disputeBond, _expected(PRICE), "captured, not current");
    }

    function test_disputingTakesTheBond() public {
        uint256 jobId = _submittedJob();
        uint256 before = token.balanceOf(clientWallet);
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        assertEq(before - token.balanceOf(clientWallet), _expected(PRICE));
        assertEq(escrow.disputeBondTotal(), _expected(PRICE));
    }

    function test_noAllowanceNoDispute() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        token.approve(address(escrow), 0);
        vm.prank(clientWallet);
        vm.expectRevert();
        escrow.dispute(jobId, keccak256("bad"));
    }

    function test_aRejectedDisputePaysTheBondToTheWorker() public {
        uint256 jobId = _disputed();
        uint256 before = token.balanceOf(workerWallet);
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        assertEq(
            token.balanceOf(workerWallet) - before,
            PRICE - _feeOf(PRICE) + _expected(PRICE),
            "payment plus the client's bond"
        );
        assertEq(escrow.disputeBondTotal(), 0);
    }

    function test_anUpheldDisputeReturnsTheBond() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _disputed();
        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        assertEq(token.balanceOf(clientWallet), before, "refund and bond both back");
        assertEq(escrow.disputeBondTotal(), 0);
    }

    function test_anExpiredDisputePaysTheWorkerAndReturnsTheBond() public {
        uint256 clientBefore = token.balanceOf(clientWallet);
        uint256 jobId = _disputed();
        uint256 workerBefore = token.balanceOf(workerWallet);
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);
        escrow.expireDispute(jobId);
        assertEq(token.balanceOf(workerWallet) - workerBefore, PRICE - _feeOf(PRICE));
        assertEq(clientBefore - token.balanceOf(clientWallet), PRICE, "paid for the job, bond back");
        assertEq(escrow.disputeBondTotal(), 0);
    }

    function test_aZeroDisputeBondMeansFreeDisputes() public {
        TaskEscrow.Config memory c = _config();
        c.disputeBondBps = 0;
        c.minDisputeBond = 0;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        uint256 jobId = _submittedJob();
        uint256 before = token.balanceOf(clientWallet);
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        assertEq(token.balanceOf(clientWallet), before);
    }

    function test_disputeBondBpsAbove100PercentIsRefused() public {
        TaskEscrow.Config memory c = _config();
        c.disputeBondBps = 10_001;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidBond.selector);
        escrow.setParams(c, feeRecipient);
    }

    /// @dev A dispute bond that cannot be paid out (the worker's wallet is
    ///      gone) is held for it, never lost and never blocking the ruling.
    function test_anUnpayableBondIsHeldNotLost() public {
        uint256 jobId = _disputed();
        vm.prank(workerOwner);
        identity.setAgentWallet(workerId, address(0));
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        assertEq(escrow.owed(workerId), PRICE - _feeOf(PRICE) + _expected(PRICE));
    }
}
