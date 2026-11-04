// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../../src/StakeVault.sol";
import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {BaseTest} from "../Base.t.sol";
import {HostileToken} from "./Adversarial.t.sol";

/// @dev A USDT-style token: `transfer` and `transferFrom` return nothing.
///      The SafeERC20 rule accepts that from a contract.
contract NoReturnToken {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
    }

    function transferFrom(address from, address to, uint256 amount) external {
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }
}

/// @dev v3: a worker's bond is locked per job while the job is open, and the
///      lock is seized for the client if the worker does not deliver. A locker
///      (the escrow) can only ever touch locks it created itself.
contract V3StakeLocksTest is BaseTest {
    address internal otherLocker = makeAddr("otherLocker");

    function setUp() public override {
        super.setUp();
        vm.startPrank(admin);
        vault.addLocker(address(this));
        vault.addLocker(otherLocker);
        vm.stopPrank();
    }

    function test_onlyALockerMayLock() public {
        vm.prank(stranger);
        vm.expectRevert(IStakeVault.NotLocker.selector);
        vault.lock(workerId, 1, 1);
    }

    function test_onlyTheConfigurerMayAddALocker() public {
        vm.prank(stranger);
        vm.expectRevert();
        vault.addLocker(stranger);
        assertTrue(vault.isLocker(address(this)));
        assertFalse(vault.isLocker(stranger));
    }

    function test_aLockReducesWhatCountsTowardHireability() public {
        _bond(workerId, workerOwner, 1_000); // MIN_STAKE + 1000
        vault.lock(workerId, 1, 1_001);
        assertEq(vault.lockedOf(workerId), 1_001);
        assertEq(vault.unlockedOf(workerId), MIN_STAKE - 1);
        assertFalse(vault.isHireable(workerId), "locked stake no longer counts");
    }

    function test_cannotLockMoreThanIsUnlocked() public {
        vault.lock(workerId, 1, MIN_STAKE);
        vm.expectRevert(IStakeVault.InsufficientBond.selector);
        vault.lock(workerId, 2, 1);
    }

    function test_cannotLockTheSameJobTwice() public {
        vault.lock(workerId, 1, 10);
        vm.expectRevert(IStakeVault.LockExists.selector);
        vault.lock(workerId, 1, 10);
    }

    function test_lockedStakeCannotBeWithdrawn() public {
        vault.lock(workerId, 1, MIN_STAKE);
        vm.prank(workerOwner);
        vm.expectRevert(IStakeVault.InsufficientBond.selector);
        vault.requestWithdraw(workerId, 1);
    }

    function test_releaseFreesTheLock() public {
        vault.lock(workerId, 1, MIN_STAKE);
        vault.release(1);
        assertEq(vault.lockedOf(workerId), 0);
        assertTrue(vault.isHireable(workerId));
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE);
    }

    function test_releasingAJobWithNoLockIsANoOp() public {
        vault.release(42);
        assertEq(vault.seize(42), 0);
    }

    function test_seizeSendsTheLockToTheLocker() public {
        vault.lock(workerId, 1, 4_000);
        uint256 before = token.balanceOf(address(this));
        uint96 taken = vault.seize(1);
        assertEq(taken, 4_000);
        assertEq(token.balanceOf(address(this)) - before, 4_000);
        assertEq(vault.bondOf(workerId), MIN_STAKE - 4_000);
        assertEq(vault.lockedOf(workerId), 0);
        assertEq(vault.seize(1), 0, "a lock is seized once");
    }

    /// @dev Locked stake always sits in `available`: `requestWithdraw` can only
    ///      move unlocked stake to `pending`. If an admin slash has already
    ///      eaten the locked part, seizing `pending` too would punish twice —
    ///      so a seizure takes only what is still in `available`.
    function test_seizeTakesOnlyWhatTheLockStillCovers() public {
        _bond(workerId, workerOwner, 5_000);
        vault.lock(workerId, 1, 5_000);
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, MIN_STAKE); // all that is unlocked
        // An admin slash takes the rest of `available`, below the lock.
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);
        vm.prank(admin);
        vault.slash(workerId, 5_000, "ruling");
        assertEq(vault.bondOf(workerId), 0);
        uint96 taken = vault.seize(1);
        assertEq(taken, 0, "the slash already took the locked part");
        (uint96 pending,) = vault.pendingOf(workerId);
        assertEq(pending, MIN_STAKE - 0, "pending was not the lock's to take");
    }

    /// @dev Never reverts, even when there is nothing left to take.
    function test_seizeNeverReverts() public {
        vault.lock(workerId, 1, MIN_STAKE);
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);
        vm.prank(admin);
        vault.slash(workerId, MIN_STAKE, "ruling");
        assertEq(vault.seize(1), 0);
        assertEq(vault.lockedOf(workerId), 0);
    }

    function test_aLockerCannotTouchAnotherLockersLock() public {
        vault.lock(workerId, 1, 1_000);
        vm.prank(otherLocker);
        assertEq(vault.seize(1), 0);
        vm.prank(otherLocker);
        vault.release(1);
        assertEq(vault.lockedOf(workerId), 1_000, "still locked");
    }

    function test_aZeroLockerIsRefused() public {
        vm.prank(admin);
        vm.expectRevert(StakeVault.ZeroAddress.selector);
        vault.addLocker(address(0));
    }

    function test_aZeroLockIsRefused() public {
        vm.expectRevert(StakeVault.ZeroAmount.selector);
        vault.lock(workerId, 1, 0);
    }

    /// @dev A token that refuses the transfer (paused, blocklisted) must not
    ///      make the seizure — and with it the client's refund — revert. The
    ///      stake stays with the agent and the lock ends.
    function test_aRefusedTransferLeavesTheStakeAndNeverReverts() public {
        HostileToken hostile = new HostileToken();
        vm.startPrank(admin);
        StakeVault v = new StakeVault(admin, ADMIN_DELAY);
        v.configure(address(hostile), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient);
        v.addLocker(address(this));
        vm.stopPrank();
        hostile.mint(workerOwner, MIN_STAKE);
        vm.startPrank(workerOwner);
        hostile.approve(address(v), MIN_STAKE);
        v.deposit(workerId, MIN_STAKE);
        vm.stopPrank();

        v.lock(workerId, 1, 5_000);
        hostile.setBlocked(true);
        assertEq(v.seize(1), 0);
        assertEq(v.bondOf(workerId), MIN_STAKE, "stake restored");
        assertEq(v.lockedOf(workerId), 0, "lock ended");
    }

    function test_seizeWorksWithATokenThatReturnsNothing() public {
        NoReturnToken t = new NoReturnToken();
        vm.startPrank(admin);
        StakeVault v = new StakeVault(admin, ADMIN_DELAY);
        v.configure(address(t), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient);
        v.addLocker(address(this));
        vm.stopPrank();
        t.mint(workerOwner, MIN_STAKE);
        vm.startPrank(workerOwner);
        t.approve(address(v), MIN_STAKE);
        v.deposit(workerId, MIN_STAKE);
        vm.stopPrank();

        v.lock(workerId, 1, 5_000);
        assertEq(v.seize(1), 5_000);
        assertEq(t.balanceOf(address(this)), 5_000);
    }

    function testFuzz_locksNeverExceedTheBond(uint96 a, uint96 b) public {
        a = uint96(bound(a, 1, MIN_STAKE));
        b = uint96(bound(b, 1, MIN_STAKE));
        vault.lock(workerId, 1, a);
        try this.lockAs(workerId, 2, b) {
            assertLe(uint256(a) + b, MIN_STAKE);
        } catch {
            assertGt(uint256(a) + b, MIN_STAKE);
        }
        assertLe(vault.lockedOf(workerId), vault.bondOf(workerId));
    }

    function lockAs(uint256 agentId, uint256 jobId, uint96 amount) external {
        vault.lock(agentId, jobId, amount);
    }
}
