// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev v3: ERC-8004's `giveFeedback` refuses a caller that is the agent's
///      owner OR an approved operator (upstream ReputationRegistryUpgradeable
///      line 110). A worker that approves the escrow as an operator of its own
///      agent therefore makes every review of itself revert — FAILED and
///      DISPUTE_LOST included. Hiring such an agent is refused, and the escrow
///      keeps its own outcome record, which no registry behaviour can suppress.
contract V3OperatorGuardTest is BaseTest {
    function test_cannotHireAWorkerThatMadeTheEscrowItsOperator() public {
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.EscrowIsOperator.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_cannotDirectPayAWorkerThatMadeTheEscrowItsOperator() public {
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.EscrowIsOperator.selector);
        escrow.directPay(clientId, workerId, MICRO, keccak256("spec"));
    }

    function test_cannotAcceptAfterMakingTheEscrowAnOperator() public {
        uint256 jobId = _createJob();
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.EscrowIsOperator.selector);
        escrow.acceptJob(jobId);
    }

    /// @dev Approving the escrow AFTER accepting still blocks the registry
    ///      write, so the failure must land in the escrow's own record.
    function test_aFailureIsRecordedEvenWhenTheRegistryRefusesTheReview() public {
        uint256 jobId = _acceptedJob();
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);

        vm.expectEmit(true, true, false, false, address(escrow));
        emit ITaskEscrow.FeedbackFailed(jobId, workerId);
        escrow.expireUndelivered(jobId);

        ITaskEscrow.AgentRecord memory r = escrow.recordOf(workerId);
        assertEq(r.failed, 1);
        assertEq(r.completed, 0);
    }

    function test_theRecordCountsEveryOutcome() public {
        // completed via approve
        uint256 a = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(a);
        // completed via directPay
        vm.prank(clientWallet);
        escrow.directPay(clientId, workerId, MICRO, keccak256("micro"));
        // dispute lost
        uint256 b = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(b, keccak256("bad"));
        vm.prank(admin);
        escrow.resolveDispute(b, false);
        // unresolved: the arbiter never ruled
        uint256 c = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(c, keccak256("bad"));
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);
        escrow.expireDispute(c);
        // cancelled jobs are nobody's outcome
        uint256 d = _createJob();
        vm.prank(clientWallet);
        escrow.cancel(d);

        ITaskEscrow.AgentRecord memory r = escrow.recordOf(workerId);
        assertEq(r.completed, 2, "completed");
        assertEq(r.failed, 0, "failed");
        assertEq(r.disputesLost, 1, "disputes lost");
        assertEq(r.unresolved, 1, "unresolved");
    }
}
