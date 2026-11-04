// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ClientReputationWriter} from "../../src/ClientReputationWriter.sol";
import {IReputationRegistry} from "../../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev Audit M-1 (2026-10-26). The operator check runs at hire and accept,
///      but an approval granted after accept and revoked before the next hire
///      made the registry refuse only the BAD review: a worker approved the
///      escrow just before `expireUndelivered`, the 0 was caught and dropped,
///      and a third-party reader of ERC-8004 saw a perfect score. A refused
///      review is now kept, anyone may write it later, and until it is written
///      the agent can neither hire nor be hired.
contract V3UnwrittenReviewTest is BaseTest {
    ClientReputationWriter internal writer;

    function setUp() public override {
        super.setUp();
        writer =
            new ClientReputationWriter(address(escrow), IReputationRegistry(address(reputation)));
        vm.prank(admin);
        escrow.setClientWriter(address(writer));
    }

    function _expireWithTheEscrowApproved() internal returns (uint256 jobId) {
        jobId = _acceptedJob();
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        identity.setApprovalFor(workerId, address(escrow), false);
        // The expiry seized the bond; top it up so only the review decides.
        _bond(workerId, workerOwner, MIN_STAKE);
    }

    function test_aRefusedFailureIsKeptNotDropped() public {
        _expireWithTheEscrowApproved();
        assertEq(escrow.unwrittenReviews(workerId), 1);
        assertEq(reputation.getLastIndex(workerId, address(escrow)), 0, "registry refused it");
    }

    function test_theToggleNoLongerBuysACleanScore() public {
        _expireWithTheEscrowApproved();
        vm.prank(clientWallet);
        vm.expectRevert(abi.encodeWithSelector(ITaskEscrow.ReviewUnwritten.selector, workerId));
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);

        vm.prank(clientWallet);
        vm.expectRevert(abi.encodeWithSelector(ITaskEscrow.ReviewUnwritten.selector, workerId));
        escrow.directPay(clientId, workerId, MICRO, keccak256("micro"));
    }

    function test_aJobOfferedBeforeTheToggleCannotBeAccepted() public {
        vm.prank(clientWallet);
        uint256 offered =
            escrow.createJob(clientId, workerId, PRICE, keccak256("early"), 1 hours, WORK_WINDOW);
        _expireWithTheEscrowApproved();
        vm.prank(workerWallet);
        vm.expectRevert(abi.encodeWithSelector(ITaskEscrow.ReviewUnwritten.selector, workerId));
        escrow.acceptJob(offered);
    }

    function test_anyoneWritesTheKeptReviewOnceTheRegistryTakesIt() public {
        uint256 jobId = _expireWithTheEscrowApproved();

        vm.expectEmit(true, true, false, false, address(escrow));
        emit ITaskEscrow.FeedbackRetried(jobId, workerId);
        vm.prank(stranger);
        escrow.retryFeedback(jobId, false);

        assertEq(escrow.unwrittenReviews(workerId), 0);
        assertEq(reputation.getLastIndex(workerId, address(escrow)), 1, "the 0 is on the registry");
        (uint64 count, int128 value,) =
            reputation.getSummary(workerId, _only(address(escrow)), "agentx", "settled");
        assertEq(count, 1);
        assertEq(value, 0, "the FAILED review, not a success");

        // and the worker is hireable again
        _createJob();
    }

    function test_aRetryWhileTheRegistryStillRefusesChangesNothing() public {
        uint256 jobId = _acceptedJob();
        identity.setApprovalFor(workerId, address(escrow), true);
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);

        vm.expectRevert();
        escrow.retryFeedback(jobId, false);
        assertEq(escrow.unwrittenReviews(workerId), 1);
    }

    function test_aReviewIsWrittenOnlyOnce() public {
        uint256 jobId = _expireWithTheEscrowApproved();
        escrow.retryFeedback(jobId, false);
        vm.expectRevert(ITaskEscrow.NothingToRetry.selector);
        escrow.retryFeedback(jobId, false);
    }

    function test_nothingToRetryForAJobWhoseReviewsLanded() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);
        vm.expectRevert(ITaskEscrow.NothingToRetry.selector);
        escrow.retryFeedback(jobId, false);
        vm.expectRevert(ITaskEscrow.NothingToRetry.selector);
        escrow.retryFeedback(jobId, true);
    }

    /// @dev The client side: approve the writer as operator while a dispute
    ///      is pending, so the rejected-dispute 0 is refused.
    function test_aClientCannotHideARejectedDisputeEither() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        identity.setApprovalFor(clientId, address(writer), true);
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        identity.setApprovalFor(clientId, address(writer), false);

        assertEq(escrow.unwrittenReviews(clientId), 1);
        assertEq(reputation.getLastIndex(clientId, address(writer)), 0);

        // the client cannot hire again until its review is written
        vm.prank(clientWallet);
        vm.expectRevert(abi.encodeWithSelector(ITaskEscrow.ReviewUnwritten.selector, clientId));
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);

        escrow.retryFeedback(jobId, true);
        (uint64 count, int128 value,) =
            reputation.getSummary(clientId, _only(address(writer)), "", "");
        assertEq(count, 1);
        assertEq(value, 0);
        assertEq(escrow.unwrittenReviews(clientId), 0);
    }

    /// @dev A registry outage is the other way a review is refused: it is
    ///      kept the same way and written when the registry is back.
    function test_aReviewRefusedByAnOutageIsWrittenWhenTheRegistryIsBack() public {
        uint256 jobId = _submittedJob();
        reputation.setShouldRevert(true);
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(escrow.unwrittenReviews(workerId), 1);
        assertEq(escrow.unwrittenReviews(clientId), 1);

        reputation.setShouldRevert(false);
        escrow.retryFeedback(jobId, false);
        escrow.retryFeedback(jobId, true);
        assertEq(escrow.unwrittenReviews(workerId), 0);
        assertEq(escrow.unwrittenReviews(clientId), 0);
        assertEq(reputation.getLastIndex(workerId, address(escrow)), 1);
        assertEq(reputation.getLastIndex(clientId, address(writer)), 1);
    }

    function _only(address a) internal pure returns (address[] memory list) {
        list = new address[](1);
        list[0] = a;
    }
}
