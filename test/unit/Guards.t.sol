// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev Covers the guard branches the lifecycle tests never reach. These are
///      the paths that only execute when something is misconfigured — exactly
///      the ones nobody exercises until production.
contract GuardsTest is BaseTest {
    function _cfg(uint16 feeBps) internal pure returns (TaskEscrow.Config memory) {
        return _cfgWith(FAST_PATH_MAX, feeBps);
    }

    // ───────────────────────── configuration ─────────────────────────

    function test_cannotConfigureTwice() public {
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.AlreadyConfigured.selector);
        escrow.configure(
            address(token),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _cfg(FEE_BPS)
        );
    }

    function test_configureRejectsExcessiveFee() public {
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.FeeTooHigh.selector);
        fresh.configure(
            address(token),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _cfg(1001)
        );
    }

    function test_setParamsRejectsExcessiveFee() public {
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.FeeTooHigh.selector);
        escrow.setParams(_cfg(1001), feeRecipient);
    }

    function test_unconfiguredEscrowRefusesWork() public {
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.prank(clientWallet);
        vm.expectRevert(TaskEscrow.NotConfigured.selector);
        fresh.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_unconfiguredVaultRefusesDeposits() public {
        StakeVault fresh = new StakeVault(admin, ADMIN_DELAY);
        vm.expectRevert(StakeVault.NotConfigured.selector);
        fresh.deposit(workerId, 1);
    }

    function test_vaultCannotBeConfiguredTwice() public {
        vm.prank(admin);
        vm.expectRevert(StakeVault.AlreadyConfigured.selector);
        vault.configure(
            address(token), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient
        );
    }

    function test_onlyConfigurerMaySetParams() public {
        vm.prank(stranger);
        vm.expectRevert();
        escrow.setParams(_cfg(FEE_BPS), feeRecipient);
    }

    // ───────────────────────── windows ─────────────────────────

    function test_zeroWindowsRejected() public {
        vm.startPrank(clientWallet);
        vm.expectRevert(TaskEscrow.WindowTooLong.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), 0, WORK_WINDOW);

        vm.expectRevert(TaskEscrow.WindowTooLong.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, 0);
        vm.stopPrank();
    }

    /// @dev A caller-supplied window must be clamped, or a client could set a
    ///      1000-year work deadline and lock its own funds past any refund.
    function test_oversizedWindowsRejected() public {
        vm.startPrank(clientWallet);
        vm.expectRevert(TaskEscrow.WindowTooLong.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), 1 days + 1, WORK_WINDOW);

        vm.expectRevert(TaskEscrow.WindowTooLong.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, 7 days + 1);
        vm.stopPrank();
    }

    // ───────────────────────── zero fee ─────────────────────────

    /// @dev At 1% an amount below 100 base units rounds the fee to zero. The
    ///      worker must still be paid in full and no zero-value transfer made.
    /// @dev v1 tested that dust paid the worker in full with no fee — which
    ///      is exactly how a review could be bought for the price of gas. Both
    ///      paths now refuse anything below `minJobAmount`.
    function test_dustIsRefusedOnBothPaths() public {
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(ITaskEscrow.AmountBelowMinimum.selector, 99, MIN_JOB_AMOUNT)
        );
        escrow.directPay(clientId, workerId, 99, keccak256("s"));

        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(ITaskEscrow.AmountBelowMinimum.selector, 50, MIN_JOB_AMOUNT)
        );
        escrow.createJob(clientId, workerId, 50, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_directPayZeroAmountRejected() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.ZeroAmount.selector);
        escrow.directPay(clientId, workerId, 0, keccak256("s"));
    }

    function test_directPaySelfDealingRejected() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.SelfDealing.selector);
        escrow.directPay(clientId, clientId, MICRO, keccak256("s"));
    }

    function test_directPayRequiresAHireableWorker() public {
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.NotHireable.selector);
        escrow.directPay(clientId, workerId, MICRO, keccak256("s"));
    }

    function test_directPayFromWrongWalletRejected() public {
        vm.prank(stranger);
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.directPay(clientId, workerId, MICRO, keccak256("s"));
    }

    // ───────────────────────── pause ─────────────────────────

    function test_unpauseRestoresNewJobs() public {
        vm.prank(admin);
        escrow.pause();
        vm.prank(admin);
        escrow.unpause();

        vm.prank(clientWallet);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_pauseAlsoBlocksTheFastPath() public {
        vm.prank(admin);
        escrow.pause();
        vm.prank(clientWallet);
        vm.expectRevert();
        escrow.directPay(clientId, workerId, MICRO, keccak256("s"));
    }

    function test_onlyAdminMayPause() public {
        vm.prank(stranger);
        vm.expectRevert();
        escrow.pause();
    }

    // ───────────────────────── vault guards ─────────────────────────

    function test_zeroAmountsRejectedEverywhere() public {
        vm.expectRevert(StakeVault.ZeroAmount.selector);
        vault.deposit(workerId, 0);

        vm.prank(workerOwner);
        vm.expectRevert(StakeVault.ZeroAmount.selector);
        vault.requestWithdraw(workerId, 0);

        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);
        vm.prank(admin);
        vm.expectRevert(StakeVault.ZeroAmount.selector);
        vault.slash(workerId, 0, "x");
    }

    function test_cannotSlashMoreThanTheWholeBond() public {
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);

        vm.prank(admin);
        vm.expectRevert(IStakeVault.InsufficientBond.selector);
        vault.slash(workerId, MIN_STAKE + 1, "too much");
    }

    function test_raisingMinStakeCanUnlistAnExistingAgent() public {
        assertTrue(vault.isHireable(workerId));
        vm.prank(admin);
        vault.setMinStake(MIN_STAKE * 2);
        assertFalse(vault.isHireable(workerId), "governance can raise the bar");
    }

    function test_onlyConfigurerMaySetMinStake() public {
        vm.prank(stranger);
        vm.expectRevert();
        vault.setMinStake(1);
    }

    function test_withdrawByNonOwnerRejected() public {
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);
        vm.warp(block.timestamp + WITHDRAW_DELAY);

        vm.prank(stranger);
        vm.expectRevert(IStakeVault.NotAgentOwner.selector);
        vault.withdraw(workerId);
    }

    // ───────────────────────── dispute guards ─────────────────────────

    function test_cannotDisputeAJobThatWasNeverSubmitted() public {
        uint256 jobId = _createJob();
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.dispute(jobId, keccak256("nothing to dispute"));
    }

    function test_cannotResolveAJobThatWasNeverDisputed() public {
        uint256 jobId = _submittedJob();
        vm.prank(admin);
        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.resolveDispute(jobId, true);
    }

    function test_onlyClientMayDispute() public {
        uint256 jobId = _submittedJob();
        vm.prank(workerWallet);
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.dispute(jobId, keccak256("x"));
    }
}
