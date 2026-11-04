// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {BaseTest} from "../Base.t.sol";

contract StakeVaultTest is BaseTest {
    function test_bondMakesAnAgentHireable() public {
        uint256 newId;
        vm.prank(stranger);
        newId = identity.register("ipfs://new", stranger);

        assertFalse(vault.isHireable(newId), "unbonded agent is not hireable");
        _bond(newId, stranger, MIN_STAKE);
        assertTrue(vault.isHireable(newId));
    }

    function test_belowMinStakeIsNotHireable() public {
        uint256 newId;
        vm.prank(stranger);
        newId = identity.register("ipfs://new", stranger);
        _bond(newId, stranger, MIN_STAKE - 1);
        assertFalse(vault.isHireable(newId), "one unit short must not qualify");
    }

    function test_anyoneMayTopUpButOnlyTheOwnerMayWithdraw() public {
        _bond(workerId, stranger, MIN_STAKE); // a stranger funding is fine
        assertEq(vault.bondOf(workerId), MIN_STAKE * 2);

        vm.prank(stranger);
        vm.expectRevert(IStakeVault.NotAgentOwner.selector);
        vault.requestWithdraw(workerId, MIN_STAKE);
    }

    /// @dev The delay is the whole point: without it an agent could take a
    ///      large job and pull its bond in the same block.
    function test_withdrawRequiresTheCooldown() public {
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);

        vm.prank(workerOwner);
        vm.expectRevert(IStakeVault.WithdrawNotReady.selector);
        vault.withdraw(workerId);

        vm.warp(block.timestamp + WITHDRAW_DELAY);
        uint256 before = token.balanceOf(workerOwner);
        vm.prank(workerOwner);
        vault.withdraw(workerId);
        assertEq(token.balanceOf(workerOwner) - before, MIN_STAKE);
    }

    function test_requestingWithdrawalImmediatelyStopsNewWork() public {
        assertTrue(vault.isHireable(workerId));
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);
        // An agent on its way out should stop receiving work at once, not
        // after the cooldown.
        assertFalse(vault.isHireable(workerId));
    }

    function test_cannotWithdrawMoreThanBonded() public {
        vm.prank(workerOwner);
        vm.expectRevert(IStakeVault.InsufficientBond.selector);
        vault.requestWithdraw(workerId, MIN_STAKE + 1);
    }

    function test_withdrawWithoutRequestReverts() public {
        vm.prank(workerOwner);
        vm.expectRevert(IStakeVault.NothingRequested.selector);
        vault.withdraw(workerId);
    }

    /// @dev Requesting a withdrawal must not be an escape hatch from slashing.
    function test_slashReachesPendingFunds() public {
        // Cache the role first: vm.prank applies to the NEXT call, and
        // vault.SLASHER_ROLE() would otherwise consume it.
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);

        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE); // all of it pending
        assertEq(vault.bondOf(workerId), 0);

        vm.prank(admin);
        vault.slash(workerId, MIN_STAKE, "fraud");

        (uint96 pending,) = vault.pendingOf(workerId);
        assertEq(pending, 0, "pending funds were slashable");

        vm.warp(block.timestamp + WITHDRAW_DELAY);
        vm.prank(workerOwner);
        vm.expectRevert(IStakeVault.NothingRequested.selector);
        vault.withdraw(workerId);
    }

    function test_slashRequiresTheRole() public {
        vm.prank(stranger);
        vm.expectRevert();
        vault.slash(workerId, 1, "nope");
    }

    /// @dev The role exists; nobody holds it in the MVP. Automatic slashing is
    ///      explicitly out of scope, and this records that fact.
    function test_noSlasherIsGrantedByDefault() public view {
        assertFalse(vault.hasRole(vault.SLASHER_ROLE(), admin), "admin must not slash by default");
        assertFalse(vault.hasRole(vault.SLASHER_ROLE(), stranger));
    }

    function testFuzz_bondAccountingNeverDrifts(uint96 a, uint96 b) public {
        a = uint96(bound(a, 1, 1e18));
        b = uint96(bound(b, 1, 1e18));

        uint256 newId;
        vm.prank(stranger);
        newId = identity.register("ipfs://f", stranger);

        _bond(newId, stranger, a);
        _bond(newId, stranger, b);
        assertEq(vault.bondOf(newId), uint256(a) + b);

        vm.prank(stranger);
        vault.requestWithdraw(newId, a);
        (uint96 pending,) = vault.pendingOf(newId);
        assertEq(uint256(vault.bondOf(newId)) + pending, uint256(a) + b, "no value lost");
    }
}
