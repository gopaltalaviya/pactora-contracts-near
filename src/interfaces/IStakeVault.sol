// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IIdentityRegistry} from "./IERC8004.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";

/// @title IStakeVault — bonds keyed by ERC-8004 agentId
/// @notice ERC-8004 has no custody, so listings would be free. A bond with a
///         withdrawal delay is what makes a listing cost something, which is
///         the direct answer to the sybil and dormant-registration findings in
///         the ERC-8004 empirical study (docs/09 §2).
interface IStakeVault {
    /// @notice `from` added `amount` to `agentId`'s bond.
    event Deposited(uint256 indexed agentId, address indexed from, uint96 amount);
    /// @notice The owner began unbonding; withdrawable from `unlockAt`.
    event WithdrawRequested(uint256 indexed agentId, uint96 amount, uint64 unlockAt);
    /// @notice The owner stopped unbonding; the amount counts toward the bond again.
    event WithdrawCancelled(uint256 indexed agentId, uint96 amount);
    /// @notice Unbonded funds left the vault.
    event Withdrawn(uint256 indexed agentId, address indexed to, uint96 amount);
    /// @notice Part of a bond was taken and sent to the slash recipient.
    event Slashed(uint256 indexed agentId, uint96 amount, bytes32 reason);
    /// @notice `locker` may now lock, release and seize its own job locks.
    event LockerAdded(address indexed locker);
    /// @notice `locker` may take no new locks.
    event LockerRemoved(address indexed locker);
    /// @notice Part of a bond is held for an open job.
    event Locked(
        address indexed locker, uint256 indexed jobId, uint256 indexed agentId, uint96 amount
    );
    /// @notice A job's lock ended without loss.
    event Released(
        address indexed locker, uint256 indexed jobId, uint256 indexed agentId, uint96 amount
    );
    /// @notice A job's lock was taken and sent to the locker. `amount` can be
    ///         less than was locked, down to zero, if a slash got there first.
    event Seized(
        address indexed locker, uint256 indexed jobId, uint256 indexed agentId, uint96 amount
    );

    error NotAgentOwner();
    error InsufficientBond();
    error WithdrawNotReady();
    error NothingRequested();
    /// @dev One request at a time: a second would reset the first one's clock.
    error WithdrawAlreadyPending();
    error NotLocker();
    error LockExists();

    /// @notice Add to an agent's bond. Anyone may; only the owner may withdraw.
    /// @param agentId the agent
    /// @param amount base units
    function deposit(uint256 agentId, uint96 amount) external;

    /// @notice Start the cooldown. Without it an agent could take a large job
    ///         and pull its bond in the same block.
    /// @param agentId the agent
    /// @param amount base units
    function requestWithdraw(uint256 agentId, uint96 amount) external;

    /// @notice Return a pending withdrawal to the bond.
    /// @param agentId the agent
    function cancelWithdraw(uint256 agentId) external;

    /// @notice Pay out a withdrawal whose cooldown has passed, to the owner.
    /// @param agentId the agent
    function withdraw(uint256 agentId) external;

    /// @notice Take part of a bond. Slasher only.
    /// @param agentId the agent
    /// @param amount base units
    /// @param reason commitment to why
    function slash(uint256 agentId, uint96 amount, bytes32 reason) external;

    /// @param agentId the agent
    /// @return the bond that counts toward hireability
    function bondOf(uint256 agentId) external view returns (uint96);

    /// @param agentId the agent
    /// @return true when the UNLOCKED bond >= minStake; AGENTX will not route
    ///         work otherwise
    function isHireable(uint256 agentId) external view returns (bool);

    /// @notice Hold part of an agent's unlocked bond for `jobId`. Locker only.
    /// @param agentId the worker
    /// @param jobId the locker's own job id
    /// @param amount base units, non-zero, at most the unlocked bond
    function lock(uint256 agentId, uint256 jobId, uint96 amount) external;

    /// @notice End the caller's lock for `jobId` without loss. A job the
    ///         caller holds no lock for is a no-op, so this never reverts.
    /// @param jobId the caller's own job id
    function release(uint256 jobId) external;

    /// @notice Take the caller's lock for `jobId` and send it to the caller.
    ///         Never reverts: a missing lock, a lock a slash already ate, or a
    ///         token that refuses the transfer all return what was taken (0).
    /// @param jobId the caller's own job id
    /// @return taken base units sent to the caller
    function seize(uint256 jobId) external returns (uint96 taken);

    /// @param agentId the agent
    /// @return the sum of its open locks
    function lockedOf(uint256 agentId) external view returns (uint96);

    /// @param agentId the agent
    /// @return the bond not held by any lock
    function unlockedOf(uint256 agentId) external view returns (uint96);

    /// @return the bond token
    function token() external view returns (IERC20);

    /// @return the ERC-8004 identity registry bonds are keyed on
    function identityRegistry() external view returns (IIdentityRegistry);
}
