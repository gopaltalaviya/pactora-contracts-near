// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IERC1271} from "openzeppelin-contracts/interfaces/IERC1271.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/token/ERC20/utils/SafeERC20.sol";
import {MessageHashUtils} from "openzeppelin-contracts/utils/cryptography/MessageHashUtils.sol";
import {SafeCast} from "openzeppelin-contracts/utils/math/SafeCast.sol";

/// @title AgentAccountCore — the spend controls shared by every agent wallet
///
/// @notice Skyfire, Nevermined and Coinbase Agentic Wallets all offer spend
///         controls, but off-chain and custodially: you trust their server.
///         These caps are verifiable by anyone and survive a full compromise
///         of the AGENTX signer — even with the hot key, an attacker cannot
///         exceed the daily cap or call anything the owner never allowlisted.
///
///         The human never approves individual payments — that is the whole
///         point of an autonomous agent — but keeps the two powers that matter:
///         **revoke** and **sweep**.
///
///         Two wallets share this code: `AgentAccount`, a clone the factory
///         creates (funds move in), and `AgentDelegate`, which an existing EOA
///         delegates to with EIP-7702 (funds stay where they are). They differ
///         only in who the owner is and how the owner signs.
///
/// @dev Spend is measured as the ACTUAL balance delta of `token` across the
///      call, not decoded from calldata. Calldata decoding has to know every
///      function signature it might meet and silently misreads anything new;
///      a balance delta cannot be fooled by an unfamiliar call.
///
///      Only `token` is measured. The allowlist is therefore per (target,
///      selector): allowing `transfer` on the payment token does not allow
///      `transfer` on any other contract, whose balance no cap would see. And
///      no selector that grants a standing allowance can ever be allowed —
///      allowances are granted by the owner via `setAllowance`, so every spend
///      shows up as a delta while the call is running.
///
///      The daily cap is ROLLING (v3): no 24-hour period ever sees more than
///      `dailyCap`. v2 used a fixed window, which let a full cap be spent just
///      before it reset and another just after — 2x in two minutes. Spend is
///      kept in 25 hourly buckets with a running total, so each spend is O(1)
///      amortised however many micro-payments an agent makes; the price is
///      that a spend counts for between 24 and 25 hours, so the cap errs safe
///      by up to one hour. `nextReleaseAt` says when capacity next returns.
///
///      All state lives in ONE ERC-7201 namespace, including the reentrancy
///      lock. An EIP-7702 delegate runs in the EOA's storage, which another
///      delegate may already have written at slot 0, 1, 2…; sequential
///      storage would read that as an owner, a token, a cap. A namespace no
///      other contract uses cannot collide.
abstract contract AgentAccountCore is IERC1271 {
    using SafeERC20 for IERC20;

    /// @param perTaskCap most one call may spend
    /// @param dailyCap most one window may spend
    /// @param allowlistOnly when false, a selector allowed for `address(0)`
    ///        is allowed on every target
    struct Policy {
        uint128 perTaskCap;
        uint128 dailyCap;
        bool allowlistOnly;
    }

    struct SessionKey {
        uint64 expiry;
        uint128 budget;
        uint128 spent;
        /// The `keyEpoch` it was granted in; any other epoch means revoked.
        uint64 epoch;
    }

    /// @custom:storage-location erc7201:agentx.storage.AgentAccount
    struct Layout {
        address owner;
        address pendingOwner;
        IERC20 token;
        Policy policy;
        /// Sum of the live buckets, as of `lastHour`.
        uint128 windowTotal;
        /// The hour (timestamp / 1h) the buckets were last rolled to.
        uint64 lastHour;
        /// Spend per hour, indexed by hour % BUCKETS.
        uint128[25] buckets;
        mapping(address => SessionKey) sessionKeys;
        mapping(address target => mapping(bytes4 selector => bool)) allowedCall;
        mapping(address target => uint256) allowedCallCount;
        /// AgentDelegate's own once-only flag (AgentAccount uses Initializable).
        bool initialized;
        /// 0 or 1: not entered; 2: entered (OpenZeppelin's values).
        uint8 lock;
        /// May extend an existing session key's expiry, and nothing else.
        address sessionManager;
        /// Bumped on an ownership transfer: retires every session key at once.
        uint64 keyEpoch;
    }

    // keccak256(abi.encode(uint256(keccak256("agentx.storage.AgentAccount")) - 1))
    //     & ~bytes32(uint256(0xff)) — checked against the formula in AgentDelegate.t.sol
    bytes32 internal constant LAYOUT_SLOT =
        0xdbd72775f83835dd5975d10e6fe6d57ffd8831b7ad0255ca6aa61f387391d100;

    /// @notice Longest life a session key may be granted, enforced on grant.
    uint64 public constant MAX_SESSION_KEY_TTL = 1 days;
    /// @dev Bucket width, and how many buckets the rolling window keeps: the
    ///      current hour plus the 24 before it, so a spend is never dropped
    ///      while it is less than 24 hours old.
    uint256 internal constant BUCKET = 1 hours;
    uint64 internal constant BUCKETS = 25;
    /// @dev EIP-712 for `isValidSignature`: the owner signs a message under a
    ///      domain naming THIS wallet, so a signature is bound to one wallet on
    ///      one chain, and a human owner can produce it with eth_signTypedData_v4.
    bytes32 internal constant DOMAIN_TYPEHASH = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );
    bytes32 internal constant MESSAGE_TYPEHASH = keccak256("AgentAccountMessage(bytes32 hash)");

    event Initialized(address indexed owner, address token);
    event PolicySet(uint128 perTaskCap, uint128 dailyCap, bool allowlistOnly);
    event SessionKeyGranted(address indexed key, uint64 expiry, uint128 budget);
    event SessionKeyRevoked(address indexed key);
    event SessionKeyExtended(address indexed key, uint64 expiry);
    event SessionManagerSet(address indexed manager);
    event CallAllowed(address indexed target, bytes4 indexed selector, bool allowed);
    event Executed(address indexed target, bytes4 selector, uint128 spent);
    event Swept(address indexed to, uint256 amount);
    event NativeSwept(address indexed to, uint256 amount);
    event AllowanceSet(address indexed spender, uint256 amount);

    error NotOwner();
    error NotAuthorized();
    error TargetNotAllowed(address target);
    error SelectorNotAllowed(bytes4 selector);
    /// @dev A selector that would hand out a standing allowance or approval.
    error ForbiddenSelector(bytes4 selector);
    error PerTaskCapExceeded(uint128 spend, uint128 cap);
    error DailyCapExceeded(uint128 spend, uint128 remaining);
    error SessionKeyExpired();
    error SessionKeyTtlInvalid(uint64 expiry, uint64 latestAllowed);
    error SessionBudgetExceeded();
    error CallFailed(bytes returndata);
    error ZeroAddress();
    error NativeTransferFailed();
    /// @dev The same call re-entered (OpenZeppelin's ReentrancyGuard error).
    error ReentrancyGuardReentrantCall();

    // ───────────────────────── who the owner is ─────────────────────────

    /// @return the address whose say is final over this wallet
    function owner() public view virtual returns (address);

    /// @dev The EIP-712 domain name a signature for this wallet is made under.
    function _domainName() internal pure virtual returns (string memory);

    modifier onlyOwner() {
        if (msg.sender != owner()) revert NotOwner();
        _;
    }

    modifier nonReentrant() {
        Layout storage s = _layout();
        if (s.lock == 2) revert ReentrancyGuardReentrantCall();
        s.lock = 2;
        _;
        s.lock = 1;
    }

    function _layout() internal pure returns (Layout storage s) {
        bytes32 slot = LAYOUT_SLOT;
        assembly {
            s.slot := slot
        }
    }

    /// @dev Set the token and caps; each wallet decides who may call this.
    function _init(address owner_, address token_, Policy calldata policy_) internal {
        if (token_ == address(0)) revert ZeroAddress();
        Layout storage s = _layout();
        s.token = IERC20(token_);
        s.policy = policy_;
        s.lastHour = _hour();
        emit Initialized(owner_, token_);
        emit PolicySet(policy_.perTaskCap, policy_.dailyCap, policy_.allowlistOnly);
    }

    // ───────────────────────── execution ─────────────────────────

    /// @notice Perform a call on the agent's behalf, within policy.
    /// @dev The checks run in this order, cheapest first:
    ///        1. caller is the owner, or an unexpired session key
    ///        2. (target, selector) is allowed, and never a forbidden selector
    ///        3. spend <= perTaskCap
    ///        4. spent in the rolling window + spend <= dailyCap
    ///      3 and 4 can only be checked AFTER the call, because the spend is a
    ///      measured balance delta. The call is reverted if it overspends, so
    ///      an over-cap call has no effect — it is checked, not merely detected.
    /// @param target the contract to call
    /// @param data the calldata
    /// @return result the call's return data
    // The spend can only be measured AFTER the call, so the cap state is
    // written after it — safe because execute is nonReentrant, and an
    // over-cap call reverts entirely.
    // slither-disable-next-line reentrancy-no-eth,reentrancy-benign
    function execute(address target, bytes calldata data)
        external
        nonReentrant
        returns (bytes memory result)
    {
        Layout storage s = _layout();
        bool isOwner = msg.sender == owner();
        if (!isOwner) _requireValidSessionKey(msg.sender);

        // The wallet's own controls are owner-only functions, reached
        // directly. Through `execute` they would be callable by a session key.
        if (target == address(this)) revert TargetNotAllowed(target);

        // The first four bytes are the selector by definition.
        // forge-lint: disable-next-line(unsafe-typecast)
        bytes4 selector = bytes4(data);
        if (_isForbidden(selector)) revert ForbiddenSelector(selector);
        if (!s.allowedCall[target][selector]) {
            if (s.policy.allowlistOnly || !s.allowedCall[address(0)][selector]) {
                if (s.allowedCallCount[target] == 0) revert TargetNotAllowed(target);
                revert SelectorNotAllowed(selector);
            }
        }

        uint256 balanceBefore = s.token.balanceOf(address(this));

        (bool ok, bytes memory ret) = target.call(data);
        if (!ok) revert CallFailed(ret);

        uint256 balanceAfter = s.token.balanceOf(address(this));
        // Checked: a delta beyond uint128 used to wrap to a small number and
        // pass the caps.
        uint128 spent =
            balanceAfter >= balanceBefore ? 0 : SafeCast.toUint128(balanceBefore - balanceAfter);

        if (spent > 0) {
            _chargeSpend(spent, isOwner ? address(0) : msg.sender);
        }

        emit Executed(target, selector, spent);
        return ret;
    }

    function _chargeSpend(uint128 spent, address sessionKey) internal {
        Layout storage s = _layout();
        if (spent > s.policy.perTaskCap) revert PerTaskCapExceeded(spent, s.policy.perTaskCap);

        // Roll the window before testing it, so expired spend never blocks work.
        uint64 hour = _hour();
        _roll(hour);

        uint128 total = s.windowTotal;
        uint128 remaining = s.policy.dailyCap > total ? s.policy.dailyCap - total : 0;
        if (spent > remaining) revert DailyCapExceeded(spent, remaining);
        s.windowTotal = total + spent;
        // A bucket index from the clock, not randomness: nothing is drawn.
        // slither-disable-next-line weak-prng
        s.buckets[hour % BUCKETS] += spent;

        if (sessionKey != address(0)) {
            SessionKey storage sk = s.sessionKeys[sessionKey];
            if (sk.spent + spent > sk.budget) revert SessionBudgetExceeded();
            sk.spent += spent;
        }
    }

    /// @dev Drop the buckets that left the window since `lastHour`. Each
    ///      bucket is cleared once, so the cost is amortised O(1) per spend;
    ///      after a long idle every bucket is cleared at most once.
    function _roll(uint64 hour) internal {
        Layout storage s = _layout();
        uint64 last = s.lastHour;
        // Hours are whole numbers; equality is exactly "same hour".
        // slither-disable-next-line incorrect-equality
        if (hour == last) return;
        if (hour - last >= BUCKETS) {
            for (uint256 i; i < BUCKETS; i++) {
                if (s.buckets[i] != 0) s.buckets[i] = 0;
            }
            s.windowTotal = 0;
        } else {
            uint128 total = s.windowTotal;
            for (uint64 h = last + 1; h <= hour; h++) {
                // slither-disable-next-line weak-prng
                uint256 i = h % BUCKETS;
                uint128 b = s.buckets[i];
                if (b != 0) {
                    total -= b;
                    s.buckets[i] = 0;
                }
            }
            s.windowTotal = total;
        }
        s.lastHour = hour;
    }

    /// @dev The window's spend as `_roll` would leave it, without writing.
    function _windowSpent() internal view returns (uint128 total) {
        Layout storage s = _layout();
        uint64 hour = _hour();
        uint64 last = s.lastHour;
        if (hour - last >= BUCKETS) return 0;
        total = s.windowTotal;
        for (uint64 h = last + 1; h <= hour; h++) {
            // slither-disable-next-line weak-prng
            total -= s.buckets[h % BUCKETS];
        }
    }

    function _hour() internal view returns (uint64) {
        return uint64(block.timestamp / BUCKET);
    }

    function _requireValidSessionKey(address key) internal view {
        Layout storage s = _layout();
        SessionKey storage sk = s.sessionKeys[key];
        if (sk.expiry == 0 || sk.epoch != s.keyEpoch) revert NotAuthorized();
        if (block.timestamp > sk.expiry) revert SessionKeyExpired();
    }

    /// @dev Selectors that grant a standing allowance or approval. Any of them
    ///      would let a spender move funds outside every cap, with no delta
    ///      visible while `execute` runs.
    function _isForbidden(bytes4 selector) internal pure returns (bool) {
        return selector == IERC20.approve.selector // approve(address,uint256)
            || selector == 0x39509351 // increaseAllowance(address,uint256)
            || selector == 0xa22cb465 // setApprovalForAll(address,bool)
            || 
            // permit(address,address,uint256,uint256,uint8,bytes32,bytes32)
            selector == 0xd505accf
            // TaskEscrow.claimOwed(uint256,address): owed funds never enter the
            // wallet, so a key that could claim them would send them anywhere,
            // outside every cap (audit I3)
            || selector == 0x2edb8967;
    }

    // ───────────────────────── owner controls ─────────────────────────

    /// @notice Replace the caps.
    /// @param policy_ the new caps
    function setPolicy(Policy calldata policy_) external onlyOwner {
        _layout().policy = policy_;
        emit PolicySet(policy_.perTaskCap, policy_.dailyCap, policy_.allowlistOnly);
    }

    /// @notice Grant a hot key a bounded, expiring right to act.
    /// @dev Rotation is a grant plus a revoke: no funds move, no downtime.
    ///      The lifetime ceiling is enforced HERE rather than left to whoever
    ///      calls this: a session key exists so that compromise of an online
    ///      signer is bounded in time (docs/04 §9.2 T7). A past expiry is
    ///      rejected too — it would log a grant for a key that never worked.
    /// @param key the hot key
    /// @param expiry unix time; within MAX_SESSION_KEY_TTL of now
    /// @param budget most the key may spend over its life
    function grantSessionKey(address key, uint64 expiry, uint128 budget) external onlyOwner {
        uint64 latest = uint64(block.timestamp) + MAX_SESSION_KEY_TTL;
        if (expiry <= block.timestamp || expiry > latest) {
            revert SessionKeyTtlInvalid(expiry, latest);
        }

        Layout storage s = _layout();
        s.sessionKeys[key] =
            SessionKey({expiry: expiry, budget: budget, spent: 0, epoch: s.keyEpoch});
        emit SessionKeyGranted(key, expiry, budget);
    }

    /// @notice Name who may keep session keys alive: a hosted renewer. It can
    ///         push out the expiry of a key the owner granted and do nothing
    ///         else, so the online process that renews never holds the owner
    ///         key (audit F1). `address(0)` removes it.
    /// @param manager the renewer
    function setSessionManager(address manager) external onlyOwner {
        _layout().sessionManager = manager;
        emit SessionManagerSet(manager);
    }

    /// @notice Push out an existing key's expiry. Owner or session manager.
    /// @dev `spent` and `budget` are kept: renewal must not refill a stolen
    ///      key's lifetime budget, which a re-grant did. A revoked key, or one
    ///      from before an ownership transfer, cannot be revived this way.
    /// @param key a key the owner granted
    /// @param expiry unix time; within MAX_SESSION_KEY_TTL of now
    function extendSessionKey(address key, uint64 expiry) external {
        Layout storage s = _layout();
        if (msg.sender != owner() && msg.sender != s.sessionManager) revert NotAuthorized();
        SessionKey storage sk = s.sessionKeys[key];
        if (sk.expiry == 0 || sk.epoch != s.keyEpoch) revert NotAuthorized();
        uint64 latest = uint64(block.timestamp) + MAX_SESSION_KEY_TTL;
        if (expiry <= block.timestamp || expiry > latest) {
            revert SessionKeyTtlInvalid(expiry, latest);
        }
        sk.expiry = expiry;
        emit SessionKeyExtended(key, expiry);
    }

    /// @notice Who may extend session keys; `address(0)` if nobody but the owner.
    function sessionManager() external view returns (address) {
        return _layout().sessionManager;
    }

    /// @dev For an ownership transfer: no grant the old owner made survives it.
    function _retireSessionKeys() internal {
        Layout storage s = _layout();
        s.keyEpoch++;
        s.sessionManager = address(0);
        emit SessionManagerSet(address(0));
    }

    /// @notice Revocable by the owner OR by the key itself, so a signer that
    ///         detects its own compromise can disarm without waiting.
    /// @param key the key to revoke
    function revokeSessionKey(address key) external {
        if (msg.sender != owner() && msg.sender != key) revert NotAuthorized();
        delete _layout().sessionKeys[key];
        emit SessionKeyRevoked(key);
    }

    /// @notice Allow or disallow one function on one contract.
    /// @param target the contract; `address(0)` is a wildcard used only when
    ///        `allowlistOnly` is false
    /// @param selector the function
    /// @param allowed whether it may be called through `execute`
    function setAllowedCall(address target, bytes4 selector, bool allowed) external onlyOwner {
        if (allowed && _isForbidden(selector)) revert ForbiddenSelector(selector);
        if (allowed && target == address(this)) revert TargetNotAllowed(target);
        Layout storage s = _layout();
        bool was = s.allowedCall[target][selector];
        if (was == allowed) return;
        s.allowedCall[target][selector] = allowed;
        if (allowed) s.allowedCallCount[target] += 1;
        else s.allowedCallCount[target] -= 1;
        emit CallAllowed(target, selector, allowed);
    }

    /// @notice Grant a standing ERC-20 allowance. Owner only, deliberately
    ///         outside `execute` so it can never be done by a session key.
    /// @param spender who may pull `token`
    /// @param amount the allowance
    function setAllowance(address spender, uint256 amount) external onlyOwner {
        _layout().token.forceApprove(spender, amount);
        emit AllowanceSet(spender, amount);
    }

    /// @notice The kill switch. Autonomy with a way out.
    /// @param to where the tokens go
    /// @param amount base units
    function sweep(address to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        _layout().token.safeTransfer(to, amount);
        emit Swept(to, amount);
    }

    /// @notice Recover the native coin. `execute` forwards none, so without
    ///         this anything sent here was locked for good.
    /// @param to where it goes
    /// @param amount wei
    function sweepNative(address payable to, uint256 amount) external onlyOwner nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        (bool ok,) = to.call{value: amount}("");
        if (!ok) revert NativeTransferFailed();
        emit NativeSwept(to, amount);
    }

    // ───────────────────────── ERC-1271 (v3) ─────────────────────────

    /// @notice What the owner signs for `hash` to be valid for this wallet.
    /// @param hash the digest a verifier asks about (e.g. the ERC-8004
    ///        registry's `setAgentWallet` digest)
    /// @return the EIP-712 digest of AgentAccountMessage(hash) for this wallet
    function accountMessageHash(bytes32 hash) public view returns (bytes32) {
        bytes32 domain = keccak256(
            abi.encode(
                DOMAIN_TYPEHASH,
                keccak256(bytes(_domainName())),
                keccak256("1"),
                block.chainid,
                address(this)
            )
        );
        return
            MessageHashUtils.toTypedDataHash(domain, keccak256(abi.encode(MESSAGE_TYPEHASH, hash)));
    }

    // ───────────────────────── views ─────────────────────────

    /// @return the token whose outflow the caps measure
    function token() external view returns (IERC20) {
        return _layout().token;
    }

    /// @return perTaskCap most one call may spend
    /// @return dailyCap most one window may spend
    /// @return allowlistOnly whether the `address(0)` wildcard is ignored
    function policy()
        external
        view
        returns (uint128 perTaskCap, uint128 dailyCap, bool allowlistOnly)
    {
        Policy storage p = _layout().policy;
        return (p.perTaskCap, p.dailyCap, p.allowlistOnly);
    }

    /// @return expiry unix time the key stops working; 0 if never granted
    /// @return budget most the key may spend over its life
    /// @return spent what it has spent
    function sessionKeys(address key)
        external
        view
        returns (uint64 expiry, uint128 budget, uint128 spent)
    {
        Layout storage s = _layout();
        SessionKey storage sk = s.sessionKeys[key];
        // A grant from a previous owner reads as never granted.
        if (sk.epoch != s.keyEpoch) return (0, 0, 0);
        return (sk.expiry, sk.budget, sk.spent);
    }

    /// @notice Whether `selector` may be called on `target` through `execute`.
    function allowedCall(address target, bytes4 selector) external view returns (bool) {
        return _layout().allowedCall[target][selector];
    }

    /// @notice How many selectors are allowed on each target.
    function allowedCallCount(address target) external view returns (uint256) {
        return _layout().allowedCallCount[target];
    }

    /// @return what was spent in the rolling window (the last 24-25 hours)
    function spentToday() external view returns (uint128) {
        return _windowSpent();
    }

    /// @return what may still be spent now without breaking the rolling cap
    function dailyRemaining() external view returns (uint128) {
        uint128 spent = _windowSpent();
        uint128 cap = _layout().policy.dailyCap;
        return cap > spent ? cap - spent : 0;
    }

    /// @return when the oldest counted spend leaves the window and its amount
    ///         becomes spendable again; 0 if nothing is counted
    function nextReleaseAt() external view returns (uint256) {
        Layout storage s = _layout();
        uint64 hour = _hour();
        uint64 last = s.lastHour;
        if (hour - last >= BUCKETS) return 0;
        uint64 first = hour >= BUCKETS - 1 ? hour - (BUCKETS - 1) : 0;
        for (uint64 h = first; h <= last; h++) {
            // slither-disable-next-line weak-prng
            if (s.buckets[h % BUCKETS] != 0) return (uint256(h) + BUCKETS) * BUCKET;
        }
        return 0;
    }

    receive() external payable {}
}
