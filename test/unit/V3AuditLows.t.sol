// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IIdentityRegistry} from "../../src/interfaces/IERC8004.sol";
import {MockIdentityRegistry} from "../../src/mocks/MockERC8004.sol";
import {BaseTest} from "../Base.t.sol";

/// @dev Low findings from the 2026-10-26 audit.
contract V3AuditLowsTest is BaseTest {
    /// @dev I-3: `claimOwed` asked the registry for the owner FIRST, so a
    ///      registry whose `ownerOf` reverts (a burned token, a bad upgrade)
    ///      locked the wallet out of its own deferred payment too.
    function test_theWalletClaimsEvenWhenOwnerOfReverts() public {
        uint256 jobId = _submittedJob();
        identity.setWalletShouldRevert(true); // the payout is deferred
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        identity.setWalletShouldRevert(false);
        uint256 owed = escrow.owed(workerId);
        assertGt(owed, 0);

        vm.mockCallRevert(
            address(identity), abi.encodeCall(IIdentityRegistry.ownerOf, (workerId)), "burned"
        );
        uint256 before = token.balanceOf(workerWallet);
        vm.prank(workerWallet);
        escrow.claimOwed(workerId, workerWallet);
        assertEq(token.balanceOf(workerWallet) - before, owed);
    }

    function test_aStrangerStillCannotClaimWhenOwnerOfReverts() public {
        uint256 jobId = _submittedJob();
        identity.setWalletShouldRevert(true);
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        identity.setWalletShouldRevert(false);
        vm.mockCallRevert(
            address(identity), abi.encodeCall(IIdentityRegistry.ownerOf, (workerId)), "burned"
        );
        vm.prank(stranger);
        vm.expectRevert();
        escrow.claimOwed(workerId, stranger);
    }

    /// @dev I-4: a vault keyed to a different identity registry would lock
    ///      and seize bonds of different agents than the escrow hires.
    function test_aVaultOnAnotherIdentityRegistryIsRefused() public {
        MockIdentityRegistry other = new MockIdentityRegistry();
        vm.startPrank(admin);
        StakeVault otherVault = new StakeVault(admin, ADMIN_DELAY);
        otherVault.configure(
            address(token), address(other), MIN_STAKE, WITHDRAW_DELAY, slashRecipient
        );
        TaskEscrow fresh = new TaskEscrow(admin, ADMIN_DELAY);
        vm.expectRevert(TaskEscrow.RegistryMismatch.selector);
        fresh.configure(
            address(token),
            address(identity),
            address(reputation),
            address(otherVault),
            feeRecipient,
            _config()
        );
        vm.stopPrank();
    }
}
