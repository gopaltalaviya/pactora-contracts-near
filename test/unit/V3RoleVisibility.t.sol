// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {BaseTest} from "../Base.t.sol";
import {IAccessControl} from "openzeppelin-contracts/access/IAccessControl.sol";

/// @dev Audit F2 (2026-10-26). Between the hand-over and the Safe's accept the
///      deployer is still admin, and could plant a second configurer, slasher
///      or locker that outlives it: AccessControl could not list who holds a
///      role, and a locker could never be removed, so the launch check passed
///      with a backdoor in place. Now every holder can be listed, lockers are
///      counted, and the admin can remove a locker.
contract V3RoleVisibilityTest is BaseTest {
    address internal backdoor = makeAddr("backdoor");

    function test_everyEscrowRoleHolderCanBeListed() public {
        bytes32 configurer = escrow.CONFIGURER_ROLE();
        assertEq(escrow.getRoleMemberCount(configurer), 1);
        assertEq(escrow.getRoleMember(configurer, 0), admin);
        assertEq(escrow.getRoleMemberCount(escrow.ARBITER_ROLE()), 1);
        assertEq(escrow.getRoleMemberCount(escrow.PAUSER_ROLE()), 1);
        assertEq(escrow.getRoleMemberCount(escrow.DEFAULT_ADMIN_ROLE()), 1);

        vm.prank(admin);
        escrow.grantRole(configurer, backdoor);
        assertEq(escrow.getRoleMemberCount(configurer), 2, "a planted holder shows up");
        assertEq(escrow.getRoleMember(configurer, 1), backdoor);
    }

    function test_everyVaultRoleHolderCanBeListed() public {
        bytes32 slasher = vault.SLASHER_ROLE();
        assertEq(vault.getRoleMemberCount(slasher), 0);
        vm.prank(admin);
        vault.grantRole(slasher, backdoor);
        assertEq(vault.getRoleMemberCount(slasher), 1);
        assertEq(vault.getRoleMember(slasher, 0), backdoor);
    }

    function test_theAdminTransferStillTakesTwoStepsAndADelay() public {
        address safe = makeAddr("safe");
        vm.prank(admin);
        escrow.beginDefaultAdminTransfer(safe);
        vm.prank(safe);
        vm.expectRevert();
        escrow.acceptDefaultAdminTransfer();
        vm.warp(block.timestamp + ADMIN_DELAY + 1);
        vm.prank(safe);
        escrow.acceptDefaultAdminTransfer();
        assertEq(escrow.defaultAdmin(), safe);
        bytes32 adminRole = escrow.DEFAULT_ADMIN_ROLE();
        assertEq(escrow.getRoleMemberCount(adminRole), 1);
        assertEq(escrow.getRoleMember(adminRole, 0), safe);
    }

    function test_lockersAreCounted() public {
        assertEq(vault.lockerCount(), 1);
        vm.startPrank(admin);
        vault.addLocker(backdoor);
        assertEq(vault.lockerCount(), 2, "a planted locker shows up");
        vault.addLocker(backdoor);
        assertEq(vault.lockerCount(), 2, "adding twice counts once");
        vm.stopPrank();
    }

    function test_theAdminRemovesAPlantedLocker() public {
        vm.prank(admin);
        vault.addLocker(backdoor);

        vm.prank(admin);
        vault.removeLocker(backdoor);
        assertFalse(vault.isLocker(backdoor));
        assertEq(vault.lockerCount(), 1);

        vm.prank(backdoor);
        vm.expectRevert(IStakeVault.NotLocker.selector);
        vault.lock(workerId, 777, 1);
    }

    function test_onlyTheAdminRemovesALocker() public {
        bytes32 configurer = vault.CONFIGURER_ROLE();
        vm.prank(admin);
        vault.grantRole(configurer, stranger);
        bytes32 adminRole = vault.DEFAULT_ADMIN_ROLE();
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, adminRole
            )
        );
        vault.removeLocker(address(escrow));
    }

    /// @dev Removing a locker stops NEW locks only: a bond it already locked
    ///      is still released when its job ends, so nothing is stuck.
    function test_aRemovedLockerStillReleasesWhatItLocked() public {
        uint256 jobId = _acceptedJob();
        assertGt(vault.lockedOf(workerId), 0);
        vm.prank(admin);
        vault.removeLocker(address(escrow));

        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        assertEq(vault.lockedOf(workerId), 0);
    }

    function test_removingSomethingThatIsNotALockerChangesNothing() public {
        vm.prank(admin);
        vault.removeLocker(backdoor);
        assertEq(vault.lockerCount(), 1);
    }
}
