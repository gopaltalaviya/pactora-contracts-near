// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev v3: a ceiling on what one escrow job may carry. Bounds the blast radius
///      of a compromised client key that has slipped its own caps, and of an
///      arbiter error on a single dispute.
contract V3MaxJobTest is BaseTest {
    function test_createJobAboveTheMaximumReverts() public {
        uint128 tooMuch = MAX_JOB_AMOUNT + 1;
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(ITaskEscrow.AmountAboveMaximum.selector, tooMuch, MAX_JOB_AMOUNT)
        );
        escrow.createJob(clientId, workerId, tooMuch, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_createJobAtTheMaximumSucceeds() public {
        vm.prank(clientWallet);
        uint256 jobId = escrow.createJob(
            clientId, workerId, MAX_JOB_AMOUNT, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW
        );
        assertEq(escrow.getJob(jobId).amount, MAX_JOB_AMOUNT);
    }

    function test_maximumCannotSitBelowTheMinimumJob() public {
        TaskEscrow.Config memory c = _config();
        c.maxJobAmount = c.minJobAmount - 1;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeeFloor.selector);
        escrow.setParams(c, feeRecipient);
    }

    /// @dev Otherwise a direct payment could exceed what escrow allows.
    function test_maximumCannotSitBelowTheFastPath() public {
        TaskEscrow.Config memory c = _config();
        c.maxJobAmount = c.fastPathMax - 1;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeeFloor.selector);
        escrow.setParams(c, feeRecipient);
    }

    /// @dev The fast path keeps its own, lower ceiling.
    function test_directPayIsBoundByFastPathMaxNotTheJobMaximum() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.AmountTooLargeForFastPath.selector);
        escrow.directPay(clientId, workerId, FAST_PATH_MAX + 1, keccak256("spec"));
    }

    function testFuzz_noJobEverExceedsTheMaximum(uint128 amount) public {
        amount = uint128(bound(amount, MIN_JOB_AMOUNT, type(uint64).max));
        token.mint(clientWallet, amount);
        vm.prank(clientWallet);
        try escrow.createJob(
            clientId, workerId, amount, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW
        ) returns (
            uint256 jobId
        ) {
            assertLe(escrow.getJob(jobId).amount, MAX_JOB_AMOUNT);
        } catch {
            assertGt(amount, MAX_JOB_AMOUNT);
        }
    }
}
