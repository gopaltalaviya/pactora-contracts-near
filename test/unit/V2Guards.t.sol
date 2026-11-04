// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccount} from "../../src/AgentAccount.sol";
import {AgentAccountCore} from "../../src/AgentAccountCore.sol";
import {AgentAccountFactory} from "../../src/AgentAccountFactory.sol";
import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";

/// @dev Refuses ETH, to prove `sweepNative` reports a failed transfer.
contract RejectsEth {
    receive() external payable {
        revert("no");
    }
}

/// @notice The guard branches v2 added. These only run when something is
///         misconfigured or hostile — exactly the paths nobody exercises
///         until production — so each one gets a test of its own.
contract V2GuardsTest is BaseTest {
    // ───────────────────────── TaskEscrow ─────────────────────────

    function test_configureRejectsAZeroDependency() public {
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.prank(admin);
        vm.expectRevert(ITaskEscrow.ZeroAddress.selector);
        fresh.configure(
            address(0),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _config()
        );
    }

    function test_minJobAmountMustBePositive() public {
        TaskEscrow.Config memory c = _config();
        c.minJobAmount = 0;
        c.minFee = 0;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeeFloor.selector);
        escrow.setParams(c, feeRecipient);
    }

    function test_fastPathCannotSitBelowTheMinimumJob() public {
        TaskEscrow.Config memory c = _config();
        c.fastPathMax = c.minJobAmount - 1;
        vm.prank(admin);
        vm.expectRevert(TaskEscrow.InvalidFeeFloor.selector);
        escrow.setParams(c, feeRecipient);
    }

    /// @dev A zero `fastPathMax` disables the fast path; it is not below anything.
    function test_aZeroFastPathMaxDisablesTheFastPath() public {
        TaskEscrow.Config memory c = _config();
        c.fastPathMax = 0;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);

        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.AmountTooLargeForFastPath.selector);
        escrow.directPay(clientId, workerId, MICRO, keccak256("s"));
    }

    function test_theFeeFloorAppliesWhenThePercentageIsLower() public {
        TaskEscrow.Config memory c = _config();
        c.minFee = 500; // 1% of MICRO is 200
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);

        vm.prank(clientWallet);
        uint256 jobId = escrow.directPay(clientId, workerId, MICRO, keccak256("s"));
        assertEq(escrow.getJob(jobId).fee, 500);
    }

    /// @dev With no percentage and no floor the fee is zero, and settling
    ///      moves nothing to the fee recipient.
    function test_aZeroFeeSettlesWithoutAFeeTransfer() public {
        TaskEscrow.Config memory c = _config();
        c.protocolFeeBps = 0;
        c.minFee = 0;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);

        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(token.balanceOf(feeRecipient), 0);
        assertEq(escrow.feesOwed(), 0);
    }

    function test_claimOwedRejectsAZeroRecipient() public {
        vm.prank(workerOwner);
        vm.expectRevert(ITaskEscrow.ZeroAddress.selector);
        escrow.claimOwed(workerId, address(0));
    }

    function test_claimFeesWithNothingOwedReverts() public {
        vm.expectRevert(ITaskEscrow.NothingOwed.selector);
        escrow.claimFees();
    }

    /// @dev The current wallet may claim too, not only the owner.
    function test_theCurrentWalletMayClaimWhatIsOwed() public {
        uint256 jobId = _submittedJob();
        identity.setWalletShouldRevert(true);
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        identity.setWalletShouldRevert(false);

        vm.prank(workerWallet);
        escrow.claimOwed(workerId, workerWallet);
        assertEq(escrow.owed(workerId), 0);
    }

    function test_pauseAndUnpauseAreAdminOnlyAndReversible() public {
        vm.prank(stranger);
        vm.expectRevert();
        escrow.pause();

        vm.prank(admin);
        escrow.pause();
        assertTrue(escrow.paused());
        vm.prank(admin);
        escrow.unpause();
        assertFalse(escrow.paused());
        _createJob(); // works again
    }

    // ───────────────────────── StakeVault ─────────────────────────

    function test_vaultConfigureGuards() public {
        vm.startPrank(admin);
        StakeVault a = new StakeVault(admin, ADMIN_DELAY);
        vm.expectRevert(StakeVault.ZeroAddress.selector);
        a.configure(address(token), address(identity), MIN_STAKE, WITHDRAW_DELAY, address(0));

        vm.expectRevert(StakeVault.ZeroAmount.selector);
        a.configure(address(token), address(identity), 0, WITHDRAW_DELAY, slashRecipient);

        vm.expectRevert(StakeVault.InvalidDelay.selector);
        a.configure(address(token), address(identity), MIN_STAKE, 0, slashRecipient);

        vm.expectRevert(StakeVault.InvalidDelay.selector);
        a.configure(address(token), address(identity), MIN_STAKE, 91 days, slashRecipient);
        vm.stopPrank();
    }

    function test_slashRecipientCanBeChangedButNeverToZero() public {
        address next = makeAddr("treasury");
        vm.prank(admin);
        vault.setSlashRecipient(next);
        assertEq(vault.slashRecipient(), next);

        vm.prank(admin);
        vm.expectRevert(StakeVault.ZeroAddress.selector);
        vault.setSlashRecipient(address(0));

        vm.prank(stranger);
        vm.expectRevert();
        vault.setSlashRecipient(stranger);
    }

    function test_setMinStakeEmitsTheChange() public {
        vm.expectEmit(address(vault));
        emit StakeVault.MinStakeUpdated(MIN_STAKE, MIN_STAKE * 2);
        vm.prank(admin);
        vault.setMinStake(MIN_STAKE * 2);
    }

    function test_cancelWithNothingPendingReverts() public {
        vm.prank(workerOwner);
        vm.expectRevert();
        vault.cancelWithdraw(workerId);
    }

    // ───────────────────────── AgentAccount / factory ─────────────────────────

    function test_factoryRejectsAZeroToken() public {
        vm.expectRevert(AgentAccountFactory.ZeroAddress.selector);
        new AgentAccountFactory(address(0));
    }

    function test_anAccountCannotBeInitialisedWithNoOwner() public {
        AgentAccountFactory f = new AgentAccountFactory(address(token));
        vm.prank(address(0));
        vm.expectRevert(AgentAccountCore.ZeroAddress.selector);
        f.createAccount(
            address(0),
            bytes32("s"),
            AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true})
        );
    }

    function _acct() internal returns (AgentAccount a, address owner_) {
        owner_ = makeAddr("acctOwner");
        AgentAccountFactory f = new AgentAccountFactory(address(token));
        vm.prank(owner_);
        a = AgentAccount(
            payable(f.createAccount(
                    owner_,
                    bytes32("s"),
                    AgentAccountCore.Policy({perTaskCap: 10, dailyCap: 10, allowlistOnly: true})
                ))
        );
    }

    function test_ownerControlsRejectZeroAddresses() public {
        (AgentAccount a, address owner_) = _acct();
        vm.startPrank(owner_);
        vm.expectRevert(AgentAccountCore.ZeroAddress.selector);
        a.sweep(address(0), 1);
        vm.expectRevert(AgentAccountCore.ZeroAddress.selector);
        a.sweepNative(payable(address(0)), 1);
        vm.expectRevert(AgentAccountCore.ZeroAddress.selector);
        a.transferOwnership(address(0));
        vm.stopPrank();
    }

    function test_sweepNativeReportsARefusedTransfer() public {
        (AgentAccount a, address owner_) = _acct();
        vm.deal(address(a), 1 ether);
        RejectsEth sink = new RejectsEth();
        vm.prank(owner_);
        vm.expectRevert(AgentAccountCore.NativeTransferFailed.selector);
        a.sweepNative(payable(address(sink)), 1 ether);
    }

    /// @dev Setting a call to the value it already has changes nothing — and
    ///      must not double-count, or disallowing it later would leave the
    ///      target still counted as allowed.
    function test_allowingTheSameCallTwiceDoesNotDoubleCount() public {
        (AgentAccount a, address owner_) = _acct();
        vm.startPrank(owner_);
        a.setAllowedCall(address(token), IERC20.transfer.selector, true);
        a.setAllowedCall(address(token), IERC20.transfer.selector, true);
        assertEq(a.allowedCallCount(address(token)), 1);
        a.setAllowedCall(address(token), IERC20.transfer.selector, false);
        assertEq(a.allowedCallCount(address(token)), 0);
        vm.stopPrank();
    }

    /// @dev With `allowlistOnly` off, a selector allowed for address(0) is
    ///      allowed on every target — and nothing else is.
    function test_wildcardSelectorsApplyOnlyWhenAllowlistOnlyIsOff() public {
        (AgentAccount a, address owner_) = _acct();
        token.mint(address(a), 100);
        vm.startPrank(owner_);
        a.setAllowedCall(address(0), IERC20.transfer.selector, true);

        vm.expectRevert(
            abi.encodeWithSelector(AgentAccountCore.TargetNotAllowed.selector, address(token))
        );
        a.execute(address(token), abi.encodeCall(IERC20.transfer, (owner_, 1)));

        a.setPolicy(AgentAccountCore.Policy({perTaskCap: 10, dailyCap: 10, allowlistOnly: false}));
        a.execute(address(token), abi.encodeCall(IERC20.transfer, (owner_, 1)));
        assertEq(token.balanceOf(owner_), 1);
        vm.stopPrank();
    }
}
