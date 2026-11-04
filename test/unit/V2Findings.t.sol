// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccount} from "../../src/AgentAccount.sol";
import {AgentAccountCore} from "../../src/AgentAccountCore.sol";
import {AgentAccountFactory} from "../../src/AgentAccountFactory.sol";
import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {BaseTest, readConfig} from "../Base.t.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";

/// @notice The audit findings of 2026-09-30, written against the v1 interface
///         FIRST and run against v1 to prove each one was real. Every test in
///         this file failed on the v1 contracts (recorded in PROGRESS,
///         Session 26); v2 makes them pass.
contract V2FindingsTest is BaseTest {
    // ─────────────── H2: reputation farming by self-dealing ───────────────

    /// @dev One owner, two agent ids. `clientId != workerId`, so v1's only
    ///      self-dealing check passed, and the owner could pay themselves for
    ///      +100 reviews.
    function test_H2_sameOwnerHireIsRejected() public {
        address secondWallet = makeAddr("clientSecondWallet");
        vm.prank(clientOwner);
        uint256 puppetId = identity.register("ipfs://puppet", secondWallet);
        _bond(puppetId, clientOwner, MIN_STAKE);

        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.SameOwner.selector);
        escrow.createJob(clientId, puppetId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    /// @dev A 1-unit payment rounded the fee to zero and still wrote a full
    ///      +100 review: a review for the price of gas.
    function test_H2_dustDirectPayIsRejected() public {
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(ITaskEscrow.AmountBelowMinimum.selector, 1, MIN_JOB_AMOUNT)
        );
        escrow.directPay(clientId, workerId, 1, keccak256("spec"));
    }

    // ─────────────── H3: a wallet that cannot receive traps funds ───────────────

    function test_H3_clearedWorkerWalletDoesNotTrapSettlement() public {
        uint256 jobId = _submittedJob();
        vm.prank(workerOwner);
        identity.setAgentWallet(workerId, address(0));
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);

        escrow.autoApprove(jobId); // v1: reverts ERC20InvalidReceiver, forever
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
    }

    function test_H3_clearedClientWalletDoesNotTrapRefund() public {
        uint256 jobId = _createJob();
        vm.prank(clientOwner);
        identity.setAgentWallet(clientId, address(0));
        vm.warp(block.timestamp + ACCEPT_WINDOW + 1);

        escrow.expireUnaccepted(jobId);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.REFUNDED));
    }

    /// @dev The identity registry is someone else's upgradeable proxy. v1's own
    ///      invariant I8 says a broken external registry must never trap a
    ///      settlement — it held for the reputation registry, not this one.
    function test_H3_revertingIdentityRegistryDoesNotTrapSettlement() public {
        uint256 jobId = _submittedJob();
        identity.setWalletShouldRevert(true);
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);

        escrow.autoApprove(jobId);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
    }

    // ─────────────── M1: parameters without bounds ───────────────

    function test_M1_zeroReviewWindowIsRejected() public {
        TaskEscrow.Config memory c = _currentConfig();
        c.reviewWindow = 0; // would silently remove every client's right to dispute
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidWindow.selector);
        escrow.setParams(c, feeRecipient);
    }

    function test_M1_zeroFeeRecipientIsRejected() public {
        // Read first: a prank applies to the NEXT call, and the config read
        // inside the argument list would take it.
        TaskEscrow.Config memory c = _currentConfig();
        vm.prank(admin);
        vm.expectRevert(ITaskEscrow.ZeroAddress.selector); // every fee transfer would revert
        escrow.setParams(c, address(0));
    }

    /// @dev The review window was read at submission, so a governance change
    ///      rewrote the terms of jobs already agreed.
    function test_M1_reviewWindowIsCapturedWhenTheJobIsCreated() public {
        uint256 jobId = _acceptedJob();
        TaskEscrow.Config memory c = _currentConfig();
        c.reviewWindow = 1 days;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);

        vm.prank(workerWallet);
        escrow.submitResult(jobId, keccak256("result"), "ipfs://result");
        assertEq(escrow.getJob(jobId).reviewDeadline, block.timestamp + REVIEW_WINDOW);
    }

    // ─────────────── StakeVault ───────────────

    /// @dev A second request silently pushed back the unlock of the first.
    function test_L4_secondWithdrawRequestWhilePendingIsRejected() public {
        _bond(workerId, workerOwner, MIN_STAKE); // 2× min
        vm.startPrank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE / 2);
        vm.expectRevert(IStakeVault.WithdrawAlreadyPending.selector);
        vault.requestWithdraw(workerId, MIN_STAKE / 2);
        vm.stopPrank();
    }

    /// @dev "Burn" only decremented storage: the tokens stayed in the vault,
    ///      owned by nobody, recoverable by nobody.
    function test_L2_slashedTokensLeaveTheVault() public {
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);

        uint256 before = token.balanceOf(address(vault));
        vm.prank(admin);
        vault.slash(workerId, 1_000_000, "misconduct");
        assertEq(before - token.balanceOf(address(vault)), 1_000_000);
    }

    function test_L3_zeroMinStakeIsRejected() public {
        vm.prank(admin);
        vm.expectRevert(StakeVault.ZeroAmount.selector); // every agent hireable with no bond
        vault.setMinStake(0);
    }

    // ─────────────── AgentAccount and factory ───────────────

    function _account(address owner_, AgentAccountCore.Policy memory p)
        internal
        returns (AgentAccountFactory f, AgentAccount a)
    {
        f = new AgentAccountFactory(address(token));
        vm.prank(owner_);
        a = AgentAccount(payable(f.createAccount(owner_, bytes32("s"), p)));
    }

    /// @dev Selectors were allowed globally, so allowing `transfer` for the
    ///      measured payment token also allowed it on every other allowed
    ///      target — whose balance the caps never measure.
    function test_M4_aSelectorAllowedForOneTargetIsNotAllowedForAnother() public {
        address owner_ = makeAddr("acctOwner");
        (, AgentAccount a) = _account(
            owner_, AgentAccountCore.Policy({perTaskCap: 10, dailyCap: 10, allowlistOnly: true})
        );
        MockUSDC other = new MockUSDC();
        other.mint(address(a), 1_000_000);

        vm.startPrank(owner_);
        a.setAllowedCall(address(token), IERC20.transfer.selector, true); // `token` only
        a.setAllowedCall(address(other), IERC20.balanceOf.selector, true); // a read, nothing more
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.SelectorNotAllowed.selector, IERC20.transfer.selector
            )
        );
        a.execute(address(other), abi.encodeCall(IERC20.transfer, (owner_, 1_000_000)));
        vm.stopPrank();
    }

    /// @dev The spend delta was cast to uint128 unchecked: a delta of
    ///      2^128 + 1 was recorded as 1 and passed a cap of 10.
    function test_L5_spendAboveUint128DoesNotWrapPastTheCap() public {
        address owner_ = makeAddr("acctOwner");
        (, AgentAccount a) = _account(
            owner_, AgentAccountCore.Policy({perTaskCap: 10, dailyCap: 10, allowlistOnly: true})
        );
        uint256 huge = uint256(type(uint128).max) + 2;
        token.mint(address(a), huge);

        vm.startPrank(owner_);
        a.setAllowedCall(address(token), IERC20.transfer.selector, true);
        vm.expectRevert(); // SafeCastOverflowedUintDowncast
        a.execute(address(token), abi.encodeCall(IERC20.transfer, (owner_, huge)));
        vm.stopPrank();
    }

    /// @dev Anyone could create the account at an owner's predicted address,
    ///      with a policy of their choosing, and make the owner's own call revert.
    function test_M6_onlyTheOwnerCanCreateTheirAccount() public {
        AgentAccountFactory f = new AgentAccountFactory(address(token));
        vm.prank(stranger);
        vm.expectRevert(AgentAccountFactory.NotOwner.selector);
        f.createAccount(
            makeAddr("victim"),
            bytes32("s"),
            AgentAccountCore.Policy({
                perTaskCap: type(uint128).max, dailyCap: type(uint128).max, allowlistOnly: false
            })
        );
    }

    // ─────────────── v2 behaviour that had no v1 equivalent ───────────────

    /// @dev H3 again, from the other side: the money held for a worker whose
    ///      wallet was cleared is claimable, by the owner, to anywhere.
    function test_H3_deferredPaymentIsClaimableByTheOwner() public {
        uint256 jobId = _submittedJob();
        vm.prank(workerOwner);
        identity.setAgentWallet(workerId, address(0));
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);

        uint256 net = PRICE - _feeOf(PRICE);
        assertEq(escrow.owed(workerId), net);
        assertEq(escrow.owedTotal(), net);
        assertGe(token.balanceOf(address(escrow)), escrow.lockedTotal() + escrow.owedTotal());

        vm.prank(stranger);
        vm.expectRevert(ITaskEscrow.NotOwedClaimer.selector);
        escrow.claimOwed(workerId, stranger);

        address rescue = makeAddr("rescue");
        vm.prank(workerOwner);
        escrow.claimOwed(workerId, rescue);
        assertEq(token.balanceOf(rescue), net);
        assertEq(escrow.owed(workerId), 0);
        assertEq(escrow.owedTotal(), 0);

        vm.prank(workerOwner);
        vm.expectRevert(ITaskEscrow.NothingOwed.selector);
        escrow.claimOwed(workerId, rescue);
    }

    /// @dev A fee recipient that cannot receive defers the fee; it never blocks
    ///      the worker's payment.
    function test_H3_undeliverableFeeIsDeferredNotBlocking() public {
        uint256 jobId = _submittedJob();
        vm.mockCallRevert(
            address(token),
            abi.encodeCall(IERC20.transfer, (feeRecipient, uint256(_feeOf(PRICE)))),
            "blacklisted"
        );
        vm.prank(clientWallet);
        escrow.approve(jobId);
        vm.clearMockedCalls();

        assertEq(escrow.feesOwed(), _feeOf(PRICE));
        escrow.claimFees();
        assertEq(token.balanceOf(feeRecipient), _feeOf(PRICE));
        assertEq(escrow.owedTotal(), 0);
    }

    /// @dev The DISPUTED state had no exit but the arbiter. Now it has a
    ///      deadline; after it anyone may settle — for the worker, who
    ///      delivered — and no feedback is written either way.
    function test_disputeExpiresAndPaysTheWorkerWithoutFeedback() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        assertEq(escrow.getJob(jobId).disputeDeadline, block.timestamp + DISPUTE_TIMEOUT);

        vm.expectRevert(ITaskEscrow.DeadlineNotPassed.selector);
        escrow.expireDispute(jobId);

        uint256 before = token.balanceOf(workerWallet);
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);
        vm.expectEmit(true, false, false, false, address(escrow));
        emit ITaskEscrow.DisputeExpired(jobId);
        escrow.expireDispute(jobId);

        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE));
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
        assertEq(_settledCount(workerId), 0, "an unresolved dispute earns no reputation");
        assertEq(escrow.lockedTotal(), 0);
    }

    function test_theArbiterCanStillRuleUntilTheDisputeIsExpired() public {
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);

        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.REFUNDED));

        vm.expectRevert(ITaskEscrow.InvalidState.selector);
        escrow.expireDispute(jobId);
    }

    /// @dev M7: the admin role cannot be handed over in one transaction.
    function test_M7_adminTransferNeedsTwoStepsAndADelay() public {
        address next = makeAddr("nextAdmin");
        bytes32 adminRole = escrow.DEFAULT_ADMIN_ROLE();

        vm.prank(admin);
        vm.expectRevert(); // AccessControlEnforcedDefaultAdminRules
        escrow.grantRole(adminRole, next);

        vm.prank(admin);
        escrow.beginDefaultAdminTransfer(next);
        vm.prank(next);
        vm.expectRevert(); // AccessControlEnforcedDefaultAdminDelay
        escrow.acceptDefaultAdminTransfer();

        vm.warp(block.timestamp + ADMIN_DELAY + 1);
        vm.prank(next);
        escrow.acceptDefaultAdminTransfer();
        assertEq(escrow.defaultAdmin(), next);
    }

    function test_L4_aPendingWithdrawalCanBeCancelled() public {
        vm.startPrank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);
        assertFalse(vault.isHireable(workerId));
        vault.cancelWithdraw(workerId);
        vm.stopPrank();

        assertTrue(vault.isHireable(workerId), "the bond counts again");
        (uint96 pending, uint64 unlockAt) = vault.pendingOf(workerId);
        assertEq(pending, 0);
        assertEq(unlockAt, 0);
    }

    function test_H2_sameWalletOnBothSidesIsRejected() public {
        vm.prank(workerOwner);
        identity.setAgentWallet(workerId, clientWallet);

        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.SelfDealing.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    function test_M1_feeFloorMustBeBelowTheMinimumJob() public {
        TaskEscrow.Config memory c = _currentConfig();
        c.minFee = c.minJobAmount;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeeFloor.selector);
        escrow.setParams(c, feeRecipient);
    }

    function test_M1_windowsAreBounded() public {
        TaskEscrow.Config memory c = _currentConfig();
        c.disputeTimeout = 366 days;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidWindow.selector);
        escrow.setParams(c, feeRecipient);
    }

    // ─────────────── helpers ───────────────

    function _currentConfig() internal view returns (TaskEscrow.Config memory c) {
        c = readConfig(escrow);
    }
}
