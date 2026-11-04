// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IIdentityRegistry} from "./interfaces/IERC8004.sol";
import {IStakeVault} from "./interfaces/IStakeVault.sol";
import {IAccessControl} from "openzeppelin-contracts/access/IAccessControl.sol";
import {
    AccessControlDefaultAdminRules
} from "openzeppelin-contracts/access/extensions/AccessControlDefaultAdminRules.sol";
import {
    AccessControl,
    AccessControlEnumerable
} from "openzeppelin-contracts/access/extensions/AccessControlEnumerable.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "openzeppelin-contracts/utils/ReentrancyGuard.sol";

/// @title StakeVault — bonds keyed by ERC-8004 agentId
/// @notice ERC-8004 has no custody, so being listed costs nothing and identity
///         is disposable. A published study of the live ecosystem found exactly
///         that: coordinated sybil registrations and dormant agents. This makes
///         being *hireable through AGENTX* cost a bond with a withdrawal delay,
///         while leaving identity itself free and open as the standard intends.
contract StakeVault is
    IStakeVault,
    AccessControlDefaultAdminRules,
    AccessControlEnumerable,
    ReentrancyGuard
{
    using SafeERC20 for IERC20;

    bytes32 public constant SLASHER_ROLE = keccak256("SLASHER_ROLE");
    bytes32 public constant CONFIGURER_ROLE = keccak256("CONFIGURER_ROLE");

    /// @dev Longest cooldown governance may impose. Beyond this a bond is a lock-up.
    uint64 public constant MAX_WITHDRAW_DELAY = 90 days;

    struct Bond {
        uint96 available; // includes locked stake; the unlocked part counts toward isHireable
        uint96 pending; // requested for withdrawal, still slashable, never locked
        uint64 unlockAt;
    }

    struct Lock {
        uint256 agentId;
        uint96 amount; // zero means no lock
    }

    IERC20 public token;
    IIdentityRegistry public identityRegistry;
    uint96 public minStake;
    uint64 public withdrawDelay;
    /// @notice Where slashed bonds go. Slashed tokens never stay stranded here.
    address public slashRecipient;
    bool public configured;

    mapping(uint256 agentId => Bond) internal _bonds;
    /// @notice Contracts allowed to lock bonds for their own jobs. Added once,
    ///         never removed: a removed locker could no longer release or
    ///         seize, which would strand the locks it holds — and with them a
    ///         settlement. Adding one is therefore a powerful admin act.
    mapping(address locker => bool) public isLocker;
    /// @notice How many lockers there are; 1 (the escrow) on a clean deploy.
    uint256 public lockerCount;
    mapping(address locker => mapping(uint256 jobId => Lock)) internal _locks;
    /// @dev Exactly the sum of the agent's open locks, across all lockers.
    mapping(uint256 agentId => uint96) internal _locked;

    /// @notice The dependencies and first parameters were set, once.
    event Configured(
        address indexed token,
        address indexed identityRegistry,
        uint96 minStake,
        uint64 withdrawDelay,
        address indexed slashRecipient
    );
    /// @notice The bond required to be hireable changed.
    event MinStakeUpdated(uint96 previous, uint96 current);
    /// @notice Slashed bonds now go to `recipient`.
    event SlashRecipientSet(address indexed recipient);

    error AlreadyConfigured();
    error NotConfigured();
    error ZeroAmount();
    error ZeroAddress();
    error InvalidDelay();
    error TokenDeliveredLess(uint256 asked, uint256 received);

    /// @param admin holds the default admin and configurer roles
    /// @param adminDelay seconds a transfer of the default admin role must wait
    constructor(address admin, uint48 adminDelay)
        AccessControlDefaultAdminRules(adminDelay, admin)
    {
        _grantRole(CONFIGURER_ROLE, admin);
    }

    /// @notice Set the dependencies and first parameters. Once only.
    /// @param token_ the bond token
    /// @param identityRegistry_ the ERC-8004 identity registry
    /// @param minStake_ bond required to be hireable; non-zero
    /// @param withdrawDelay_ cooldown, 1 second to 90 days
    /// @param slashRecipient_ where slashed bonds go
    function configure(
        address token_,
        address identityRegistry_,
        uint96 minStake_,
        uint64 withdrawDelay_,
        address slashRecipient_
    ) external onlyRole(CONFIGURER_ROLE) {
        if (configured) revert AlreadyConfigured();
        if (
            token_ == address(0) || identityRegistry_ == address(0) || slashRecipient_ == address(0)
        ) {
            revert ZeroAddress();
        }
        if (minStake_ == 0) revert ZeroAmount();
        if (withdrawDelay_ == 0 || withdrawDelay_ > MAX_WITHDRAW_DELAY) revert InvalidDelay();

        configured = true;
        token = IERC20(token_);
        identityRegistry = IIdentityRegistry(identityRegistry_);
        minStake = minStake_;
        withdrawDelay = withdrawDelay_;
        slashRecipient = slashRecipient_;
        emit Configured(token_, identityRegistry_, minStake_, withdrawDelay_, slashRecipient_);
    }

    /// @inheritdoc IStakeVault
    // Balance read around transferFrom on purpose (fee-on-transfer guard); nonReentrant.
    // slither-disable-next-line reentrancy-balance,reentrancy-benign
    function deposit(uint256 agentId, uint96 amount) external nonReentrant {
        if (!configured) revert NotConfigured();
        if (amount == 0) revert ZeroAmount();

        // Credit only what arrived. Crediting the asked-for amount under a
        // fee-on-transfer token would let one agent's bond be withdrawn out
        // of another's deposit.
        uint256 received = token.balanceOf(address(this));
        token.safeTransferFrom(msg.sender, address(this), amount);
        received = token.balanceOf(address(this)) - received;
        if (received != amount) revert TokenDeliveredLess(amount, received);

        _bonds[agentId].available += amount;
        emit Deposited(agentId, msg.sender, amount);
    }

    /// @inheritdoc IStakeVault
    /// @dev One request at a time. A second used to reset the first one's
    ///      unlock time silently; now the owner cancels and re-requests, which
    ///      says what it does.
    function requestWithdraw(uint256 agentId, uint96 amount) external {
        _onlyAgentOwner(agentId);
        Bond storage b = _bonds[agentId];
        if (amount == 0) revert ZeroAmount();
        if (b.pending != 0) revert WithdrawAlreadyPending();
        // Locked stake stays put until its job ends.
        if (_unlockedOf(agentId) < amount) revert InsufficientBond();

        b.available -= amount;
        b.pending = amount;
        b.unlockAt = uint64(block.timestamp) + withdrawDelay;

        emit WithdrawRequested(agentId, amount, b.unlockAt);
    }

    /// @inheritdoc IStakeVault
    function cancelWithdraw(uint256 agentId) external {
        _onlyAgentOwner(agentId);
        Bond storage b = _bonds[agentId];
        uint96 amount = b.pending;
        if (amount == 0) revert NothingRequested();

        b.pending = 0;
        b.unlockAt = 0;
        b.available += amount;
        emit WithdrawCancelled(agentId, amount);
    }

    /// @inheritdoc IStakeVault
    function withdraw(uint256 agentId) external nonReentrant {
        address owner = _onlyAgentOwner(agentId);
        Bond storage b = _bonds[agentId];

        uint96 amount = b.pending;
        if (amount == 0) revert NothingRequested();
        if (block.timestamp < b.unlockAt) revert WithdrawNotReady();

        b.pending = 0;
        b.unlockAt = 0;

        token.safeTransfer(owner, amount);
        emit Withdrawn(agentId, owner, amount);
    }

    /// @inheritdoc IStakeVault
    /// @dev Takes from `available` first and then from `pending`, so requesting
    ///      a withdrawal is not an escape hatch from being slashed. The tokens
    ///      go to `slashRecipient`; v1 only decremented storage and left them
    ///      in the vault, owned by nobody.
    ///      No role holder is granted SLASHER_ROLE in the MVP — the capability
    ///      exists, automatic slashing does not.
    function slash(uint256 agentId, uint96 amount, bytes32 reason)
        external
        onlyRole(SLASHER_ROLE)
        nonReentrant
    {
        Bond storage b = _bonds[agentId];
        if (amount == 0) revert ZeroAmount();
        if (uint256(b.available) + b.pending < amount) revert InsufficientBond();

        uint96 fromAvailable = amount <= b.available ? amount : b.available;
        b.available -= fromAvailable;
        b.pending -= (amount - fromAvailable);
        if (b.pending == 0) b.unlockAt = 0;

        token.safeTransfer(slashRecipient, amount);
        emit Slashed(agentId, amount, reason);
    }

    // ─────────────────────────── job locks (v3) ───────────────────────────

    /// @notice Allow `locker` to lock bonds for its own jobs.
    /// @param locker the escrow (or a future kernel)
    function addLocker(address locker) external onlyRole(CONFIGURER_ROLE) {
        if (locker == address(0)) revert ZeroAddress();
        if (isLocker[locker]) return;
        isLocker[locker] = true;
        lockerCount++;
        emit LockerAdded(locker);
    }

    /// @notice Stop `locker` taking NEW locks; the admin only (audit F2: a
    ///         locker planted during the hand-over window could otherwise
    ///         never be removed). Locks it already holds are still released
    ///         or seized by their job's end, keyed by the locker, so no bond
    ///         is stuck.
    /// @param locker the locker to remove
    function removeLocker(address locker) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (!isLocker[locker]) return;
        isLocker[locker] = false;
        lockerCount--;
        emit LockerRemoved(locker);
    }

    /// @inheritdoc IStakeVault
    function lock(uint256 agentId, uint256 jobId, uint96 amount) external {
        if (!isLocker[msg.sender]) revert NotLocker();
        if (amount == 0) revert ZeroAmount();
        Lock storage l = _locks[msg.sender][jobId];
        if (l.amount != 0) revert LockExists();
        if (_unlockedOf(agentId) < amount) revert InsufficientBond();

        l.agentId = agentId;
        l.amount = amount;
        _locked[agentId] += amount;
        emit Locked(msg.sender, jobId, agentId, amount);
    }

    /// @inheritdoc IStakeVault
    /// @dev Keyed by the caller, so nobody can release another locker's lock;
    ///      for anyone else the lookup is empty and nothing happens.
    function release(uint256 jobId) external {
        Lock memory l = _locks[msg.sender][jobId];
        if (l.amount == 0) return;
        delete _locks[msg.sender][jobId];
        _locked[l.agentId] -= l.amount;
        emit Released(msg.sender, jobId, l.agentId, l.amount);
    }

    /// @inheritdoc IStakeVault
    /// @dev Takes from `available` only: locked stake always sits there, and if
    ///      an admin slash already took it, reaching into `pending` would
    ///      punish the same failure twice.
    function seize(uint256 jobId) external nonReentrant returns (uint96 taken) {
        Lock memory l = _locks[msg.sender][jobId];
        if (l.amount == 0) return 0;
        delete _locks[msg.sender][jobId];
        _locked[l.agentId] -= l.amount;

        Bond storage b = _bonds[l.agentId];
        taken = l.amount <= b.available ? l.amount : b.available;
        if (taken != 0) {
            b.available -= taken;
            // A token that refuses the transfer must not make the caller's
            // refund revert; the stake simply stays with the agent.
            if (!_tryTransfer(msg.sender, taken)) {
                b.available += taken;
                taken = 0;
            }
        }
        emit Seized(msg.sender, jobId, l.agentId, taken);
    }

    /// @inheritdoc IStakeVault
    function lockedOf(uint256 agentId) external view returns (uint96) {
        return _locked[agentId];
    }

    /// @inheritdoc IStakeVault
    function unlockedOf(uint256 agentId) external view returns (uint96) {
        return _unlockedOf(agentId);
    }

    /// @inheritdoc IStakeVault
    function bondOf(uint256 agentId) external view returns (uint96) {
        return _bonds[agentId].available;
    }

    /// @param agentId the agent
    /// @return amount pending withdrawal
    /// @return unlockAt when it becomes withdrawable
    function pendingOf(uint256 agentId) external view returns (uint96 amount, uint64 unlockAt) {
        Bond storage b = _bonds[agentId];
        return (b.pending, b.unlockAt);
    }

    /// @inheritdoc IStakeVault
    /// @dev Pending funds deliberately do NOT count: an agent that has started
    ///      unbonding is on its way out and should stop receiving work. Nor
    ///      does locked stake (v3): a worker's bond caps how much open work it
    ///      can hold at once.
    function isHireable(uint256 agentId) external view returns (bool) {
        return _unlockedOf(agentId) >= minStake;
    }

    /// @notice Change the bond required to be hireable. Never zero.
    /// @param newMinStake base units
    function setMinStake(uint96 newMinStake) external onlyRole(CONFIGURER_ROLE) {
        if (newMinStake == 0) revert ZeroAmount();
        emit MinStakeUpdated(minStake, newMinStake);
        minStake = newMinStake;
    }

    /// @notice Change where slashed bonds go.
    /// @param recipient non-zero
    function setSlashRecipient(address recipient) external onlyRole(CONFIGURER_ROLE) {
        if (recipient == address(0)) revert ZeroAddress();
        slashRecipient = recipient;
        emit SlashRecipientSet(recipient);
    }

    /// @dev Saturating: an admin slash can take `available` below the locks.
    function _unlockedOf(uint256 agentId) internal view returns (uint96) {
        uint96 available = _bonds[agentId].available;
        uint96 locked = _locked[agentId];
        return available > locked ? available - locked : 0;
    }

    /// @dev `transfer` that reports failure instead of reverting, by the
    ///      SafeERC20 rule (as TaskEscrow._tryTransfer).
    function _tryTransfer(address to, uint256 amount) internal returns (bool success) {
        bytes memory data = abi.encodeCall(IERC20.transfer, (to, amount));
        address t = address(token);
        assembly ("memory-safe") {
            success := call(gas(), t, 0, add(data, 0x20), mload(data), 0, 0x20)
            if success {
                switch returndatasize()
                case 0 { success := gt(extcodesize(t), 0) }
                default { success := and(gt(returndatasize(), 0x1f), eq(mload(0), 1)) }
            }
        }
    }

    function _onlyAgentOwner(uint256 agentId) internal view returns (address owner) {
        owner = identityRegistry.ownerOf(agentId);
        if (owner != msg.sender) revert NotAgentOwner();
    }

    // ─────────── role holders can be listed (audit F2) ───────────
    // A role granted during the hand-over window must be visible afterwards:
    // AccessControl alone cannot say who holds a role, so the launch check
    // could not see a planted holder.

    /// @inheritdoc AccessControlDefaultAdminRules
    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(AccessControlDefaultAdminRules, AccessControlEnumerable)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }

    function _grantRole(bytes32 role, address account)
        internal
        override(AccessControlDefaultAdminRules, AccessControlEnumerable)
        returns (bool)
    {
        return super._grantRole(role, account);
    }

    function _revokeRole(bytes32 role, address account)
        internal
        override(AccessControlDefaultAdminRules, AccessControlEnumerable)
        returns (bool)
    {
        return super._revokeRole(role, account);
    }

    function grantRole(bytes32 role, address account)
        public
        override(AccessControl, AccessControlDefaultAdminRules, IAccessControl)
    {
        super.grantRole(role, account);
    }

    function revokeRole(bytes32 role, address account)
        public
        override(AccessControl, AccessControlDefaultAdminRules, IAccessControl)
    {
        super.revokeRole(role, account);
    }

    function renounceRole(bytes32 role, address account)
        public
        override(AccessControl, AccessControlDefaultAdminRules, IAccessControl)
    {
        super.renounceRole(role, account);
    }

    function _setRoleAdmin(bytes32 role, bytes32 adminRole)
        internal
        override(AccessControl, AccessControlDefaultAdminRules)
    {
        super._setRoleAdmin(role, adminRole);
    }
}
