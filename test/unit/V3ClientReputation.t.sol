// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ClientReputationWriter} from "../../src/ClientReputationWriter.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IReputationRegistry} from "../../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {MockReputationRegistry} from "../../src/mocks/MockERC8004.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev v3: clients are rated too, from a SEPARATE writer address, so a reader
///      filtering on the escrow gets worker reviews only and one filtering on
///      the writer gets client reviews only — even with empty tags.
contract V3ClientReputationTest is BaseTest {
    ClientReputationWriter internal writer;

    function setUp() public override {
        super.setUp();
        writer =
            new ClientReputationWriter(address(escrow), IReputationRegistry(address(reputation)));
        vm.prank(admin);
        escrow.setClientWriter(address(writer));
    }

    function _clientReviews() internal view returns (uint64) {
        return reputation.getLastIndex(clientId, address(writer));
    }

    function test_onlyTheEscrowMayWrite() public {
        vm.prank(stranger);
        vm.expectRevert(ClientReputationWriter.NotEscrow.selector);
        writer.rate(clientId, 100, bytes32(0));
    }

    function test_theWriterIsSetOnceAndMustBelongToThisEscrow() public {
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.AlreadyConfigured.selector);
        escrow.setClientWriter(address(writer));

        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.WriterMismatch.selector);
        fresh.setClientWriter(address(writer)); // belongs to `escrow`
    }

    function test_aZeroWriterIsRefused() public {
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.prank(admin);
        vm.expectRevert(ITaskEscrow.ZeroAddress.selector);
        fresh.setClientWriter(address(0));
    }

    function test_aWriterOnAnotherRegistryIsRefused() public {
        MockReputationRegistry other = new MockReputationRegistry(identity);
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.startPrank(admin);
        fresh.configure(
            address(token),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _config()
        );
        ClientReputationWriter w =
            new ClientReputationWriter(address(fresh), IReputationRegistry(address(other)));
        vm.expectRevert(TaskEscrow.WriterMismatch.selector);
        fresh.setClientWriter(address(w));
        vm.stopPrank();
    }

    function test_aClientThatApprovesIsRatedWell() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(_clientReviews(), 1);
        assertEq(escrow.clientRecordOf(clientId).paid, 1);
        // and the worker review still comes from the escrow
        assertEq(reputation.getLastIndex(workerId, address(escrow)), 1);
        assertEq(reputation.getLastIndex(workerId, address(writer)), 0, "never mixed");
    }

    function test_aDirectPaymentRatesTheClient() public {
        vm.prank(clientWallet);
        escrow.directPay(clientId, workerId, MICRO, keccak256("m"));
        assertEq(_clientReviews(), 1);
        assertEq(escrow.clientRecordOf(clientId).paid, 1);
    }

    function test_aRejectedDisputeCountsAgainstTheClient() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        assertEq(_clientReviews(), 1);
        assertEq(escrow.clientRecordOf(clientId).disputesRejected, 1);
    }

    function test_anUpheldDisputeCountsForTheClient() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        assertEq(_clientReviews(), 1);
        assertEq(escrow.clientRecordOf(clientId).disputesUpheld, 1);
    }

    /// @dev Owner decision: a silent client is recorded, not penalised.
    function test_aSilentClientIsRecordedNeutrally() public {
        uint256 jobId = _submittedJob();
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        assertEq(_clientReviews(), 1);
        assertEq(escrow.clientRecordOf(clientId).silent, 1);
        assertEq(escrow.clientRecordOf(clientId).paid, 0);
    }

    function test_anUndeliveredJobDoesNotRateTheClient() public {
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        assertEq(_clientReviews(), 0);
    }

    /// @dev Same self-block as the worker side: a client that makes the
    ///      writer its operator would make every review of itself revert.
    function test_cannotHireForAClientThatMadeTheWriterItsOperator() public {
        identity.setApprovalFor(clientId, address(writer), true);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.EscrowIsOperator.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    /// @dev Two reviews in one transaction: the caller must leave gas for both.
    function test_theClientReviewCannotBeStarvedEither() public {
        uint256 jobId = _submittedJob();
        bytes memory data = abi.encodeCall(TaskEscrow.approve, (jobId));
        bool succeeded;
        for (uint256 g = 60_000; g <= 2_000_000; g += 8_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(clientWallet);
            (bool ok,) = address(escrow).call{gas: g}(data);
            uint64 reviews = _clientReviews();
            vm.revertToState(snap);
            assertFalse(ok && reviews == 0, "approved without the client review");
            if (ok) succeeded = true;
        }
        assertTrue(succeeded);
    }

    function test_noWriterNoClientReviews() public {
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.startPrank(admin);
        fresh.configure(
            address(token),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _config()
        );
        vault.addLocker(address(fresh));
        vm.stopPrank();
        vm.prank(clientWallet);
        token.approve(address(fresh), type(uint256).max);
        vm.prank(clientWallet);
        fresh.directPay(clientId, workerId, MICRO, keccak256("m"));
        assertEq(fresh.clientRecordOf(clientId).paid, 1, "the record still counts");
    }
}
