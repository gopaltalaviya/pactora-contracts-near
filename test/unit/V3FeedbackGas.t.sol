// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev Stands in for the canonical ERC-8004 Reputation Registry on Monad
///      testnet (0x8004B663…), whose first `giveFeedback` from a new client
///      measured 278,485 gas by eth_estimateGas on 2026-10-25 — about 254k
///      inside the call. Our MockReputationRegistry is far cheaper, which is
///      why the v2 tests never saw the starvation below.
contract GasHungryRegistry {
    uint256 internal constant BURN = 254_000;
    mapping(uint256 agentId => uint256) public written;

    function giveFeedback(
        uint256 agentId,
        int128,
        uint8,
        string calldata,
        string calldata,
        string calldata,
        string calldata,
        bytes32
    ) external {
        uint256 start = gasleft();
        if (start <= BURN) {
            // Out of gas, the way a real registry runs out: consume everything.
            while (true) {}
        }
        while (start - gasleft() < BURN) {}
        written[agentId] += 1;
    }
}

/// @dev v3: a caller must not be able to choose a gas limit that makes the
///      escrow settle or refund while silently dropping the review.
///      `expireUndelivered` and `autoApprove` are permissionless — the worker
///      who failed can call the first; `approve` and `directPay` are the
///      client's, and a client may want the worker paid but unreviewed.
contract V3FeedbackGasTest is BaseTest {
    GasHungryRegistry internal hungry;
    TaskEscrow internal esc;

    function setUp() public override {
        super.setUp();
        hungry = new GasHungryRegistry();
        vm.startPrank(admin);
        esc = new TaskEscrow(admin, ADMIN_DELAY);
        esc.configure(
            address(token),
            address(identity),
            address(hungry),
            address(vault),
            feeRecipient,
            _config()
        );
        vault.addLocker(address(esc));
        vm.stopPrank();
        vm.prank(clientWallet);
        token.approve(address(esc), type(uint256).max);
    }

    function _job() internal returns (uint256 jobId) {
        vm.prank(clientWallet);
        jobId =
            esc.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
        vm.prank(workerWallet);
        esc.acceptJob(jobId);
    }

    /// @dev Sweeps the caller's gas limit. At every limit the call must either
    ///      revert, or succeed WITH the review written. Never succeed without it.
    ///      And with enough gas it must succeed, so a fix that simply always
    ///      reverts cannot pass.
    function _sweep(address caller, bytes memory data) internal returns (uint256 droppedAt) {
        bool succeeded;
        for (uint256 g = 60_000; g <= 1_400_000; g += 4_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(caller);
            (bool ok,) = address(esc).call{gas: g}(data);
            uint256 reviews = hungry.written(workerId);
            vm.revertToState(snap);
            if (ok && reviews == 0) return g;
            if (ok) succeeded = true;
        }
        assertTrue(succeeded, "never succeeded, even with ample gas");
    }

    function test_theFailedWorkerCannotStarveItsOwnFailedReview() public {
        uint256 jobId = _job();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        uint256 droppedAt =
            _sweep(workerWallet, abi.encodeCall(TaskEscrow.expireUndelivered, (jobId)));
        assertEq(droppedAt, 0, "expireUndelivered succeeded without writing the FAILED review");
    }

    function test_aClientCannotPayButStarveTheSuccessReview() public {
        uint256 jobId = _job();
        vm.prank(workerWallet);
        esc.submitResult(jobId, keccak256("result"), "ipfs://result");
        uint256 droppedAt = _sweep(clientWallet, abi.encodeCall(TaskEscrow.approve, (jobId)));
        assertEq(droppedAt, 0, "approve succeeded without writing the review");
    }

    function test_autoApproveCannotBeStarved() public {
        uint256 jobId = _job();
        vm.prank(workerWallet);
        esc.submitResult(jobId, keccak256("result"), "ipfs://result");
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        uint256 droppedAt = _sweep(stranger, abi.encodeCall(TaskEscrow.autoApprove, (jobId)));
        assertEq(droppedAt, 0, "autoApprove succeeded without writing the review");
    }

    function test_directPayCannotBeStarved() public {
        uint256 droppedAt = _sweep(
            clientWallet,
            abi.encodeCall(TaskEscrow.directPay, (clientId, workerId, MICRO, keccak256("spec")))
        );
        assertEq(droppedAt, 0, "directPay succeeded without writing the review");
    }
}

/// @dev The same sweep against MockReputationRegistry, so the record shows
///      whether the cheaper mock could ever have exposed the starvation.
contract V3FeedbackGasMockTest is BaseTest {
    function test_theFailedWorkerCannotStarveItsOwnFailedReviewOnTheMock() public {
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        bytes memory data = abi.encodeCall(TaskEscrow.expireUndelivered, (jobId));
        bool succeeded;
        for (uint256 g = 60_000; g <= 1_400_000; g += 4_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(workerWallet);
            (bool ok,) = address(escrow).call{gas: g}(data);
            uint64 reviews = reputation.getLastIndex(workerId, address(escrow));
            vm.revertToState(snap);
            assertFalse(ok && reviews == 0, "succeeded without the review");
            if (ok) succeeded = true;
        }
        assertTrue(succeeded, "never succeeded, even with ample gas");
    }
}

contract V3FeedbackGasParamsTest is BaseTest {
    function test_feedbackGasBelowTheFloorIsRefused() public {
        TaskEscrow.Config memory c = _config();
        c.feedbackGas = 99_999;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeedbackGas.selector);
        escrow.setParams(c, feeRecipient);
    }

    function test_feedbackGasAboveTheCeilingIsRefused() public {
        TaskEscrow.Config memory c = _config();
        c.feedbackGas = 2_000_001;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeedbackGas.selector);
        escrow.setParams(c, feeRecipient);
    }

    /// @dev Too little gas is a revert the caller can fix, not a silent drop.
    function test_tooLittleGasRevertsWithTheAmountNeeded() public {
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        vm.prank(stranger);
        (bool ok, bytes memory ret) = address(escrow).call{gas: 300_000}(
            abi.encodeCall(TaskEscrow.expireUndelivered, (jobId))
        );
        assertFalse(ok);
        assertEq(bytes4(ret), TaskEscrow.InsufficientGasForFeedback.selector);
    }
}
