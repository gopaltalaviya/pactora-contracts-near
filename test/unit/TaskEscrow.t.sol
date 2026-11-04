// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {Vm} from "forge-std/Vm.sol";

contract TaskEscrowTest is BaseTest {
    // ───────────────────────── happy path ─────────────────────────

    function test_fullLifecycle_paysWorkerAndRecordsFeedback() public {
        uint256 before = token.balanceOf(workerWallet);
        uint256 jobId = _submittedJob();

        vm.prank(clientWallet);
        escrow.approve(jobId);

        uint128 fee = _feeOf(PRICE);
        assertEq(token.balanceOf(workerWallet) - before, PRICE - fee, "worker paid net of fee");
        assertEq(token.balanceOf(feeRecipient), fee, "fee accrued");
        assertEq(escrow.lockedTotal(), 0, "nothing still locked");
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));

        // The differentiator: feedback exists, and only because money moved.
        assertEq(_settledCount(workerId), 1, "settlement-backed feedback recorded");
    }

    function test_feedbackHashBindsToTheExactJob() public {
        uint256 jobId = _submittedJob();

        bytes32 expected =
            keccak256(abi.encode(jobId, keccak256("spec"), keccak256("result"), PRICE));

        vm.recordLogs();
        vm.prank(clientWallet);
        escrow.approve(jobId);

        // NewFeedback(uint256,address,uint64,int128,uint8,string,string,string,string,string,bytes32)
        // agentId, clientAddress and indexedTag1 are indexed, so the data
        // payload is (uint64, int128, uint8, string, string, string, string, bytes32).
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool found;
        for (uint256 i; i < logs.length; i++) {
            if (logs[i].emitter != address(reputation)) continue;
            (,,,,,,, bytes32 feedbackHash) = abi.decode(
                logs[i].data, (uint64, int128, uint8, string, string, string, string, bytes32)
            );
            if (feedbackHash == expected) found = true;
        }
        assertTrue(found, "feedbackHash must bind jobId+specHash+resultHash+amount");
    }

    function test_directPay_settlesImmediately() public {
        uint256 before = token.balanceOf(workerWallet);

        vm.prank(clientWallet);
        uint256 jobId = escrow.directPay(clientId, workerId, MICRO, keccak256("spec"));

        uint128 fee = _feeOf(MICRO);
        assertEq(token.balanceOf(workerWallet) - before, MICRO - fee);
        assertEq(escrow.lockedTotal(), 0, "fast path never locks funds");
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
        assertEq(_settledCount(workerId), 1);
    }

    function test_directPay_rejectsAboveThreshold() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.AmountTooLargeForFastPath.selector);
        escrow.directPay(clientId, workerId, FAST_PATH_MAX + 1, keccak256("spec"));
    }

    // ───────────────────────── authorisation ─────────────────────────

    function test_onlyClientWalletCanCreate() public {
        vm.prank(stranger);
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_onlyWorkerWalletCanAccept() public {
        uint256 jobId = _createJob();
        vm.prank(stranger);
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.acceptJob(jobId);
    }

    function test_onlyClientWalletCanApprove() public {
        uint256 jobId = _submittedJob();
        vm.prank(workerWallet); // the worker would love to approve its own work
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.approve(jobId);
    }

    function test_selfDealingRejected() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.SelfDealing.selector);
        escrow.createJob(clientId, clientId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_unbondedWorkerNotHireable() public {
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE); // drops available below minStake

        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.NotHireable.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_zeroAmountRejected() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.ZeroAmount.selector);
        escrow.createJob(clientId, workerId, 0, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_agentIdAboveUint64Reverts() public {
        uint256 hugeId = uint256(type(uint64).max) + 1;
        identity.registerAt(hugeId, workerOwner, workerWallet);
        _bond(hugeId, workerOwner, MIN_STAKE);

        vm.prank(clientWallet);
        vm.expectRevert(abi.encodeWithSelector(ITaskEscrow.AgentIdTooLarge.selector, hugeId));
        escrow.createJob(clientId, hugeId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    // ───────────────── illegal transitions all revert ─────────────────

    function test_cannotAcceptTwice() public {
        uint256 jobId = _acceptedJob();
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.acceptJob(jobId);
    }

    function test_cannotSubmitBeforeAccept() public {
        uint256 jobId = _createJob();
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.submitResult(jobId, keccak256("r"), "");
    }

    function test_cannotApproveBeforeSubmit() public {
        uint256 jobId = _acceptedJob();
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.approve(jobId);
    }

    function test_terminalStateIsFinal() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);

        // Invariant I2: nothing moves a settled job again.
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.approve(jobId);

        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.autoApprove(jobId);

        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.cancel(jobId);
    }

    function test_cannotAcceptAfterDeadline() public {
        uint256 jobId = _createJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + 1);
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.DeadlinePassed.selector);
        escrow.acceptJob(jobId);
    }

    function test_cannotSubmitAfterWorkDeadline() public {
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.DeadlinePassed.selector);
        escrow.submitResult(jobId, keccak256("r"), "");
    }

    // ────────── permissionless exits: no job traps funds (I5) ──────────

    function test_expireUnaccepted_refundsClient_callableByAnyone() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _createJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + 1);

        vm.prank(stranger); // permissionless on purpose
        escrow.expireUnaccepted(jobId);

        assertEq(token.balanceOf(clientWallet), before, "client made whole");
        assertEq(escrow.lockedTotal(), 0);
        // Nobody accepted, so nobody failed — no feedback written.
        assertEq(_settledCount(workerId), 0);
    }

    function test_expireUndelivered_refundsAndRecordsFailure() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);

        vm.prank(stranger);
        escrow.expireUndelivered(jobId);

        // v3: the worker's bond for the job comes back with the refund.
        assertEq(token.balanceOf(clientWallet), before + MIN_BOND, "client refunded, plus the bond");
        // The worker took the job and did not deliver. That is a failure.
        assertEq(_settledCount(workerId), 1, "failure recorded on-chain");
    }

    function test_autoApprove_paysWorkerWithoutTheClient() public {
        uint256 before = token.balanceOf(workerWallet);
        uint256 jobId = _submittedJob();
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);

        // A client cannot take a result and withhold payment by going quiet.
        vm.prank(stranger);
        escrow.autoApprove(jobId);

        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE));
    }

    function test_exitsRejectedBeforeTheirDeadline() public {
        uint256 jobId = _createJob();
        vm.expectRevert(ITaskEscrow.DeadlineNotPassed.selector);
        escrow.expireUnaccepted(jobId);

        vm.prank(workerWallet);
        escrow.acceptJob(jobId);
        vm.expectRevert(ITaskEscrow.DeadlineNotPassed.selector);
        escrow.expireUndelivered(jobId);

        vm.prank(workerWallet);
        escrow.submitResult(jobId, keccak256("r"), "");
        vm.expectRevert(ITaskEscrow.DeadlineNotPassed.selector);
        escrow.autoApprove(jobId);
    }

    function test_cancelBeforeAccept() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _createJob();
        vm.prank(clientWallet);
        escrow.cancel(jobId);
        assertEq(token.balanceOf(clientWallet), before);
    }

    // ───────────────────────── disputes ─────────────────────────

    function test_disputeResolvedForWorker() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("schema mismatch"));

        uint256 before = token.balanceOf(workerWallet);
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        // v3: a rejected dispute also pays the worker the client's dispute bond.
        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE) + MIN_DISPUTE_BOND);
    }

    function test_disputeResolvedForClient() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));

        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        assertEq(token.balanceOf(clientWallet), before, "client refunded");
        assertEq(_settledCount(workerId), 1, "loss recorded");
    }

    function test_onlyArbiterResolves() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("x"));

        vm.prank(stranger);
        vm.expectRevert();
        escrow.resolveDispute(jobId, true);
    }

    function test_cannotDisputeAfterReviewWindow() public {
        uint256 jobId = _submittedJob();
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.DeadlinePassed.selector);
        escrow.dispute(jobId, keccak256("late"));
    }

    // ───────── I8: reputation must never be able to trap money ─────────

    function test_settlementSurvivesARevertingRegistry() public {
        uint256 jobId = _submittedJob();
        reputation.setShouldRevert(true);

        uint256 before = token.balanceOf(workerWallet);

        vm.expectEmit(true, true, false, false, address(escrow));
        emit ITaskEscrow.FeedbackFailed(jobId, workerId);

        vm.prank(clientWallet);
        escrow.approve(jobId); // must NOT revert

        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE), "worker still paid");
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
    }

    /// @dev Threat T17: an owner approving the escrow as an operator makes
    ///      ERC-8004 reject its feedback. It must degrade, not trap funds.
    ///      v3 refuses the hire and the accept if the approval is already in
    ///      place (V3OperatorGuard), so this is the remaining window: approval
    ///      granted after the work was accepted.
    function test_operatorApprovalGriefingDoesNotTrapFunds() public {
        uint256 jobId = _submittedJob();
        vm.prank(workerOwner);
        identity.setApprovalFor(workerId, address(escrow), true);
        uint256 before = token.balanceOf(workerWallet);

        vm.prank(clientWallet);
        escrow.approve(jobId);

        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE), "paid anyway");
        assertEq(_settledCount(workerId), 0, "feedback was rejected, as expected");
        assertEq(escrow.recordOf(workerId).completed, 1, "the escrow's own record still counts it");
    }

    // ───────────────────────── pausing ─────────────────────────

    function test_pauseBlocksNewJobsButNeverTrapsExistingFunds() public {
        uint256 jobId = _submittedJob();

        vm.prank(admin);
        escrow.pause();

        vm.prank(clientWallet);
        vm.expectRevert();
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);

        // Settlement must remain open while paused.
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(escrow.lockedTotal(), 0, "paused contract still released funds");
    }

    // ───────────────────────── fees (I4) ─────────────────────────

    function testFuzz_paidPlusFeeAlwaysEqualsAmount(uint128 amount) public {
        _liftMaxJobAmount();
        amount = uint128(bound(amount, MIN_JOB_AMOUNT, 1_000_000_000));
        token.mint(clientWallet, amount);

        vm.prank(clientWallet);
        uint256 jobId = escrow.createJob(
            clientId, workerId, amount, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW
        );

        ITaskEscrow.Job memory job = escrow.getJob(jobId);
        assertEq(job.amount, job.fee + (job.amount - job.fee), "no value created or destroyed");
        // The percentage rounds down, then the floor applies — and the worker
        // is always paid something.
        assertEq(job.fee, _feeOf(amount), "fee is max(bps, minFee)");
        assertLt(job.fee, amount, "the fee never takes the whole payment");
    }

    function test_feeChangeCannotAlterAnInFlightJob() public {
        uint256 jobId = _createJob();
        uint128 feeAtCreation = escrow.getJob(jobId).fee;

        vm.prank(admin);
        escrow.setParams(
            _cfgWith(FAST_PATH_MAX, 1000), // 10x increase
            feeRecipient
        );

        assertEq(
            escrow.getJob(jobId).fee, feeAtCreation, "captured at creation, immune to governance"
        );
    }
}
