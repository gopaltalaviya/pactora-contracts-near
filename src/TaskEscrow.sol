// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ClientReputationWriter} from "./ClientReputationWriter.sol";
import {IIdentityRegistry, IReputationRegistry} from "./interfaces/IERC8004.sol";
import {IStakeVault} from "./interfaces/IStakeVault.sol";
import {ITaskEscrow} from "./interfaces/ITaskEscrow.sol";
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
import {Pausable} from "openzeppelin-contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "openzeppelin-contracts/utils/ReentrancyGuard.sol";

/// @title TaskEscrow — the AGENTX job lifecycle and the sole writer of
///        settlement-backed ERC-8004 feedback
///
/// @notice ERC-8004's `giveFeedback()` is free for anyone to call, which is why
///         a published study found its reputation manipulable. This contract is
///         the answer: it writes feedback ONLY after a job has settled on-chain,
///         so a reader who filters `getSummary` to this address gets a score
///         that cannot exist without a real payment between two DIFFERENT
///         owners, of at least `minJobAmount`, that paid at least `minFee`.
///
///         That is a cost floor on manufactured reputation, not a proof of
///         honesty: an attacker with two identities and two bonds can still pay
///         themselves. What v2 guarantees is that each such review costs a real
///         fee, where v1 let one owner buy one for the price of gas.
///
/// @dev Payments never trap funds. Every payout goes through `_payAgent`, which
///      credits `owed` instead of reverting when the recipient cannot be paid —
///      a cleared wallet, a token-blacklisted address, or an identity registry
///      (someone else's upgradeable proxy) that reverts. Solvency:
///      `balance == lockedTotal + owedTotal + disputeBondTotal` (invariant I1).
contract TaskEscrow is
    ITaskEscrow,
    AccessControlDefaultAdminRules,
    AccessControlEnumerable,
    Pausable,
    ReentrancyGuard
{
    using SafeERC20 for IERC20;

    bytes32 public constant ARBITER_ROLE = keccak256("ARBITER_ROLE");
    bytes32 public constant CONFIGURER_ROLE = keccak256("CONFIGURER_ROLE");
    /// @dev v3: may `pause()` and nothing else. Resuming is the admin's.
    bytes32 public constant PAUSER_ROLE = keccak256("PAUSER_ROLE");

    uint16 internal constant BPS_DENOMINATOR = 10_000;
    uint16 internal constant MAX_FEE_BPS = 1_000; // 10%
    /// @dev Upper bound on every window. Keeps `now + window` far from uint64
    ///      overflow and lets the per-job copies live in uint32.
    uint64 internal constant MAX_WINDOW = 365 days;
    /// @dev Gas given to the identity registry when resolving a payout wallet.
    ///      Bounded so a hostile registry cannot spend the gas the fallback
    ///      (crediting `owed`) needs.
    uint256 internal constant WALLET_LOOKUP_GAS = 100_000;
    /// @dev Bounds on `Config.feedbackGas`. The canonical registry's first
    ///      write measured ~254k inside the call on Monad testnet (2026-10-25).
    uint32 internal constant MIN_FEEDBACK_GAS = 100_000;
    uint32 internal constant MAX_FEEDBACK_GAS = 2_000_000;
    /// @dev Gas kept back for what runs after the registry call: the
    ///      `FeedbackFailed` emit, the reentrancy-guard exit and the return.
    uint256 internal constant FEEDBACK_RESERVE = 20_000;

    /// @param fastPathMax largest `directPay`, base units; 0 disables the fast path
    /// @param minJobAmount smallest amount any job may carry
    /// @param minFee fee floor, charged when the percentage fee is lower
    /// @param protocolFeeBps fee in basis points, at most 10%
    /// @param reviewWindow seconds a client has to approve or dispute
    /// @param maxAcceptWindow longest accept window a client may choose
    /// @param maxWorkWindow longest work window a client may choose
    /// @param disputeTimeout seconds the arbiter has before anyone may expire a dispute
    /// @param maxJobAmount largest amount an escrow job may carry; bounds what one
    ///        compromised client key or one arbiter error can move
    /// @param feedbackGas gas given to every `giveFeedback` call; the caller
    ///        must leave enough for it, so no gas limit can drop a review
    /// @param bondBps share of the job amount locked from the worker's stake
    ///        while the job is open, at most 100%
    /// @param minBond the least any escrow job locks
    /// @param disputeBondBps share of the job amount a client posts to dispute,
    ///        at most 100%
    /// @param minDisputeBond the least a dispute costs (0 with 0 bps: free)
    struct Config {
        uint128 fastPathMax;
        uint128 minJobAmount;
        uint128 minFee;
        uint16 protocolFeeBps;
        uint64 reviewWindow;
        uint64 maxAcceptWindow;
        uint64 maxWorkWindow;
        uint64 disputeTimeout;
        uint128 maxJobAmount;
        uint32 feedbackGas;
        uint16 bondBps;
        uint128 minBond;
        uint16 disputeBondBps;
        uint128 minDisputeBond;
    }

    IERC20 public token;
    IIdentityRegistry public identityRegistry;
    IReputationRegistry public reputationRegistry;
    IStakeVault public stakeVault;
    address public feeRecipient;
    Config public config;
    bool public configured;

    uint256 public nextJobId = 1;

    /// @notice Sum of `amount` across all non-terminal jobs.
    uint256 public lockedTotal;
    /// @notice Deferred payments plus deferred fees, still held here.
    uint256 public owedTotal;
    /// @notice Clients' dispute bonds held for disputes not yet ended.
    uint256 public disputeBondTotal;
    /// @notice Deferred fees, deliverable with `claimFees`.
    uint256 public feesOwed;
    /// @notice Deferred payments per agent, claimable with `claimOwed`.
    mapping(uint256 agentId => uint256) public owed;

    mapping(uint256 jobId => Job) internal _jobs;
    mapping(uint256 agentId => AgentRecord) internal _records;
    mapping(uint256 agentId => ClientRecord) internal _clientRecords;

    /// @dev A review the registry refused at settlement, kept so it can be
    ///      written later (audit M-1). `pending` because agent id 0 is valid.
    struct UnwrittenReview {
        uint256 agentId;
        int128 value;
        bool pending;
    }

    mapping(uint256 jobId => mapping(bool client => UnwrittenReview)) internal _unwritten;
    /// @inheritdoc ITaskEscrow
    mapping(uint256 agentId => uint256) public unwrittenReviews;
    /// @notice Writes client reviews from its own address (v3). Set once;
    ///         zero means clients are counted in `clientRecordOf` but not rated.
    address public clientWriter;

    /// @notice The contract's dependencies were set, once.
    event Configured(
        address indexed token,
        address indexed identityRegistry,
        address indexed reputationRegistry,
        address stakeVault
    );
    /// @notice The protocol parameters or fee recipient changed.
    event ParamsUpdated(Config config, address indexed feeRecipient);

    error AlreadyConfigured();
    error NotConfigured();
    error WindowTooLong();
    error InvalidWindow();
    error FeeTooHigh();
    error InvalidFeeFloor();
    error InvalidFeedbackGas();
    error InvalidBond();
    /// @dev The writer belongs to another escrow or writes to another registry.
    error WriterMismatch();
    /// @dev The stake vault holds a different token from the one jobs are paid in.
    error TokenMismatch();
    /// @dev The vault keys bonds on a different identity registry (audit I-4).
    error RegistryMismatch();
    /// @dev The caller's gas limit would starve the registry call, which
    ///      would settle or refund the job without its review.
    error InsufficientGasForFeedback(uint256 left, uint256 needed);
    /// @dev The token moved a different amount than it was asked to.
    error TokenDeliveredLess(uint256 asked, uint256 received);

    /// @param admin holds the default admin, configurer, arbiter and pauser roles
    /// @param adminDelay seconds a transfer of the default admin role must wait
    constructor(address admin, uint48 adminDelay)
        AccessControlDefaultAdminRules(adminDelay, admin)
    {
        _grantRole(CONFIGURER_ROLE, admin);
        _grantRole(ARBITER_ROLE, admin);
        _grantRole(PAUSER_ROLE, admin);
    }

    /// @notice Set the dependencies and the first parameters. Once only.
    /// @param token_ the payment token
    /// @param identityRegistry_ the ERC-8004 identity registry
    /// @param reputationRegistry_ the ERC-8004 reputation registry
    /// @param stakeVault_ the bond vault that decides hireability
    /// @param feeRecipient_ receives protocol fees
    /// @param config_ the protocol parameters
    function configure(
        address token_,
        address identityRegistry_,
        address reputationRegistry_,
        address stakeVault_,
        address feeRecipient_,
        Config calldata config_
    ) external onlyRole(CONFIGURER_ROLE) {
        if (configured) revert AlreadyConfigured();
        if (
            token_ == address(0) || identityRegistry_ == address(0)
                || reputationRegistry_ == address(0) || stakeVault_ == address(0)
        ) revert ZeroAddress();
        _checkParams(config_, feeRecipient_); // includes feeRecipient_ != 0
        // Seized bonds are paid to the client with the refund, in the job's token.
        if (address(IStakeVault(stakeVault_).token()) != token_) revert TokenMismatch();
        // ...and keys them on the agents this escrow hires (audit I-4).
        if (address(IStakeVault(stakeVault_).identityRegistry()) != identityRegistry_) {
            revert RegistryMismatch();
        }
        configured = true;
        token = IERC20(token_);
        identityRegistry = IIdentityRegistry(identityRegistry_);
        reputationRegistry = IReputationRegistry(reputationRegistry_);
        stakeVault = IStakeVault(stakeVault_);
        // slither-disable-next-line missing-zero-check
        feeRecipient = feeRecipient_; // checked in _checkParams
        config = config_;
        emit Configured(token_, identityRegistry_, reputationRegistry_, stakeVault_);
        emit ParamsUpdated(config_, feeRecipient_);
    }

    /// @notice Change the parameters for FUTURE jobs. A job's fee and windows
    ///         are captured at creation, so nothing here alters one in flight.
    /// @param config_ the new parameters
    /// @param feeRecipient_ the new fee recipient
    function setParams(Config calldata config_, address feeRecipient_)
        external
        onlyRole(CONFIGURER_ROLE)
    {
        _checkParams(config_, feeRecipient_);
        config = config_;
        // slither-disable-next-line missing-zero-check
        feeRecipient = feeRecipient_; // checked in _checkParams
        emit ParamsUpdated(config_, feeRecipient_);
    }

    /// @notice Attach the client-review writer. Once only.
    /// @param writer a ClientReputationWriter built for this escrow and registry
    function setClientWriter(address writer) external onlyRole(CONFIGURER_ROLE) {
        if (clientWriter != address(0)) revert AlreadyConfigured();
        if (writer == address(0)) revert ZeroAddress();
        ClientReputationWriter w = ClientReputationWriter(writer);
        if (
            w.escrow() != address(this)
                || address(w.reputationRegistry()) != address(reputationRegistry)
        ) {
            revert WriterMismatch();
        }
        clientWriter = writer;
    }

    // ─────────────────────────── escrow path ───────────────────────────

    /// @inheritdoc ITaskEscrow
    // The balance is read around transferFrom ON PURPOSE (the fee-on-transfer
    // guard), and the function is nonReentrant.
    // slither-disable-next-line reentrancy-balance
    function createJob(
        uint256 clientAgentId,
        uint256 workerAgentId,
        uint128 amount,
        bytes32 specHash,
        uint64 acceptWindow,
        uint64 workWindow
    ) external whenNotPaused nonReentrant returns (uint256 jobId) {
        _validateHire(clientAgentId, workerAgentId, amount);
        // Escrow path only: `directPay` has its own, lower ceiling.
        if (amount > config.maxJobAmount) revert AmountAboveMaximum(amount, config.maxJobAmount);
        if (acceptWindow == 0 || workWindow == 0) revert WindowTooLong();
        if (acceptWindow > config.maxAcceptWindow || workWindow > config.maxWorkWindow) {
            revert WindowTooLong();
        }

        uint128 fee = _feeOf(amount);
        uint64 acceptDeadline = uint64(block.timestamp) + acceptWindow;

        jobId = nextJobId++;
        _jobs[jobId] = Job({
            clientAgentId: _toU64(clientAgentId),
            workerAgentId: _toU64(workerAgentId),
            amount: amount,
            fee: fee,
            acceptDeadline: acceptDeadline,
            // Absolute from the start, so the worker knows its real deadline
            // before accepting rather than after.
            workDeadline: acceptDeadline + workWindow,
            reviewDeadline: 0,
            state: JobState.CREATED,
            // Bounded by MAX_WINDOW in _checkParams, so these casts are exact.
            reviewWindow: uint32(config.reviewWindow),
            disputeTimeout: uint32(config.disputeTimeout),
            disputeDeadline: 0,
            specHash: specHash,
            resultHash: bytes32(0),
            workerBond: _bondOf(amount),
            disputeBond: _disputeBondOf(amount)
        });

        lockedTotal += amount;

        // Trust the balance, not the call. A fee-on-transfer or deflationary
        // token delivers less than it was asked for, which would leave the
        // escrow owing more than it holds — invariant I1, broken silently.
        uint256 received = token.balanceOf(address(this));
        token.safeTransferFrom(msg.sender, address(this), amount);
        received = token.balanceOf(address(this)) - received;
        if (received != amount) revert TokenDeliveredLess(amount, received);

        emit JobCreated(jobId, clientAgentId, workerAgentId, amount, fee, specHash, acceptDeadline);
    }

    /// @inheritdoc ITaskEscrow
    /// @dev Locks `workerBond` from the worker's unlocked stake; the vault
    ///      reverts if there is not enough, so the worker cannot accept and the
    ///      client's exit is `expireUnaccepted`.
    function acceptJob(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        _onlyWorkerWallet(job);
        if (job.state != JobState.CREATED) revert InvalidState();
        if (block.timestamp > job.acceptDeadline) revert DeadlinePassed();
        _notOperator(job.workerAgentId);
        _reviewsWritten(job.workerAgentId);

        job.state = JobState.ACCEPTED;
        emit JobAccepted(jobId, job.workDeadline);
        // The vault is ours and its lock is all-or-nothing.
        // slither-disable-next-line reentrancy-events
        if (job.workerBond != 0) stakeVault.lock(job.workerAgentId, jobId, uint96(job.workerBond));
    }

    /// @inheritdoc ITaskEscrow
    function submitResult(uint256 jobId, bytes32 resultHash, string calldata resultURI) external {
        Job storage job = _jobs[jobId];
        _onlyWorkerWallet(job);
        if (job.state != JobState.ACCEPTED) revert InvalidState();
        if (block.timestamp > job.workDeadline) revert DeadlinePassed();

        job.state = JobState.SUBMITTED;
        job.resultHash = resultHash;
        job.reviewDeadline = uint64(block.timestamp) + job.reviewWindow;

        emit ResultSubmitted(jobId, resultHash, resultURI);
    }

    /// @inheritdoc ITaskEscrow
    // Writes after the registry call record ITS refusal (audit M-1), so they
    // cannot come first; every path is nonReentrant and the call is budgeted.
    // slither-disable-next-line reentrancy-no-eth
    function approve(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        _onlyClientWallet(job);
        if (job.state != JobState.SUBMITTED) revert InvalidState();
        _clientRecords[job.clientAgentId].paid++;
        _settle(jobId, job, Outcome.SUCCESS, true);
        _rateClient(jobId, job.clientAgentId, 100, job);
    }

    /// @inheritdoc ITaskEscrow
    /// @dev Takes the client's `disputeBond` (v3). Same fee-on-transfer guard
    ///      as `createJob`: a token that delivers less would break I1.
    // slither-disable-next-line reentrancy-balance
    function dispute(uint256 jobId, bytes32 reasonHash) external nonReentrant {
        Job storage job = _jobs[jobId];
        _onlyClientWallet(job);
        if (job.state != JobState.SUBMITTED) revert InvalidState();
        if (block.timestamp > job.reviewDeadline) revert DeadlinePassed();

        job.state = JobState.DISPUTED;
        job.disputeDeadline = uint64(block.timestamp) + job.disputeTimeout;
        uint128 bond = job.disputeBond;
        disputeBondTotal += bond;
        emit JobDisputed(jobId, reasonHash);

        if (bond != 0) {
            uint256 received = token.balanceOf(address(this));
            token.safeTransferFrom(msg.sender, address(this), bond);
            received = token.balanceOf(address(this)) - received;
            if (received != bond) revert TokenDeliveredLess(bond, received);
        }
    }

    /// @inheritdoc ITaskEscrow
    // Writes after the registry call record ITS refusal (audit M-1), so they
    // cannot come first; every path is nonReentrant and the call is budgeted.
    // slither-disable-next-line reentrancy-no-eth
    function resolveDispute(uint256 jobId, bool forWorker)
        external
        onlyRole(ARBITER_ROLE)
        nonReentrant
    {
        Job storage job = _jobs[jobId];
        if (job.state != JobState.DISPUTED) revert InvalidState();

        emit DisputeResolved(jobId, forWorker);
        if (forWorker) _clientRecords[job.clientAgentId].disputesRejected++;
        else _clientRecords[job.clientAgentId].disputesUpheld++;
        // A rejected dispute costs the client its bond; an upheld one does not.
        _settleDisputeBond(jobId, job, forWorker ? job.workerAgentId : job.clientAgentId);
        if (forWorker) {
            _settle(jobId, job, Outcome.SUCCESS, true);
        } else {
            _refund(jobId, job, "dispute", Outcome.DISPUTE_LOST, true);
        }
        _rateClient(jobId, job.clientAgentId, forWorker ? int128(0) : int128(100), job);
    }

    /// @inheritdoc ITaskEscrow
    function cancel(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        _onlyClientWallet(job);
        if (job.state != JobState.CREATED) revert InvalidState();
        // No outcome recorded: nobody accepted, so nobody failed.
        _refund(jobId, job, "cancelled", Outcome.SUCCESS, false);
    }

    // ───────────── permissionless exits (liveness, invariant I5) ─────────────
    // Every non-terminal state has one. No job can hold funds forever because
    // a counterparty — or the arbiter — went offline.

    /// @inheritdoc ITaskEscrow
    function expireUnaccepted(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        if (job.state != JobState.CREATED) revert InvalidState();
        if (block.timestamp <= job.acceptDeadline) revert DeadlineNotPassed();
        _refund(jobId, job, "unaccepted", Outcome.SUCCESS, false);
    }

    /// @inheritdoc ITaskEscrow
    function expireUndelivered(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        if (job.state != JobState.ACCEPTED) revert InvalidState();
        if (block.timestamp <= job.workDeadline) revert DeadlineNotPassed();
        // The worker accepted and did not deliver. That is a failure, and it
        // is recorded as one.
        _refund(jobId, job, "undelivered", Outcome.FAILED, true);
    }

    /// @inheritdoc ITaskEscrow
    // Writes after the registry call record ITS refusal (audit M-1), so they
    // cannot come first; every path is nonReentrant and the call is budgeted.
    // slither-disable-next-line reentrancy-no-eth
    function autoApprove(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        if (job.state != JobState.SUBMITTED) revert InvalidState();
        if (block.timestamp <= job.reviewDeadline) revert DeadlineNotPassed();
        // Settles without the client, so a client cannot take a result and
        // withhold payment by going quiet. Recorded, not penalised (owner, v3).
        _clientRecords[job.clientAgentId].silent++;
        _settle(jobId, job, Outcome.SUCCESS, true);
        _rateClient(jobId, job.clientAgentId, 50, job);
    }

    /// @inheritdoc ITaskEscrow
    /// @dev Pays the WORKER. The result is delivered and revealed; refunding on
    ///      timeout would make "dispute, then wait out a slow arbiter" a way to
    ///      get work for free — the exploit `autoApprove` exists to prevent.
    ///      No feedback is written, so a disputed result never earns
    ///      reputation by default. The arbiter can still rule until this runs.
    function expireDispute(uint256 jobId) external nonReentrant {
        Job storage job = _jobs[jobId];
        if (job.state != JobState.DISPUTED) revert InvalidState();
        if (block.timestamp <= job.disputeDeadline) revert DeadlineNotPassed();
        emit DisputeExpired(jobId);
        // The arbiter's silence is not the client's fault: its bond comes back.
        _settleDisputeBond(jobId, job, job.clientAgentId);
        _settle(jobId, job, Outcome.UNRESOLVED, false);
    }

    // ─────────────────────────── fast path ───────────────────────────

    /// @inheritdoc ITaskEscrow
    /// @dev All-or-nothing: the funds never rest here, so a recipient that
    ///      cannot be paid simply reverts the payment — nothing is stranded.
    // Writes after the registry call record ITS refusal (audit M-1), so they
    // cannot come first; every path is nonReentrant and the call is budgeted.
    // slither-disable-next-line reentrancy-no-eth
    function directPay(
        uint256 clientAgentId,
        uint256 workerAgentId,
        uint128 amount,
        bytes32 specHash
    ) external whenNotPaused nonReentrant returns (uint256 jobId) {
        address workerWallet = _validateHire(clientAgentId, workerAgentId, amount);
        if (amount > config.fastPathMax) revert AmountTooLargeForFastPath();

        uint128 fee = _feeOf(amount);
        uint128 paid = amount - fee;

        jobId = nextJobId++;
        _jobs[jobId] = Job({
            clientAgentId: _toU64(clientAgentId),
            workerAgentId: _toU64(workerAgentId),
            amount: amount,
            fee: fee,
            acceptDeadline: uint64(block.timestamp),
            workDeadline: uint64(block.timestamp),
            reviewDeadline: uint64(block.timestamp),
            state: JobState.SETTLED,
            reviewWindow: 0,
            disputeTimeout: 0,
            disputeDeadline: 0,
            specHash: specHash,
            resultHash: bytes32(0),
            workerBond: 0,
            disputeBond: 0
        });
        _records[workerAgentId].completed++;
        _clientRecords[clientAgentId].paid++;

        token.safeTransferFrom(msg.sender, workerWallet, paid);
        token.safeTransferFrom(msg.sender, feeRecipient, fee);

        emit DirectPaid(jobId, clientAgentId, workerAgentId, amount, fee, specHash);
        emit JobSettled(jobId, paid, fee, Outcome.SUCCESS);

        _recordFeedback(jobId, workerAgentId, Outcome.SUCCESS, _jobs[jobId]);
        _rateClient(jobId, clientAgentId, 100, _jobs[jobId]);
    }

    // ─────────────────────────── deferred payments ───────────────────────────

    /// @inheritdoc ITaskEscrow
    /// @dev The agent's OWNER may claim, as may its current wallet. The owner
    ///      can already point the wallet anywhere, so letting it choose `to`
    ///      grants no new power — and a canonical registry clears the wallet
    ///      on transfer, which would otherwise leave nobody able to claim.
    function claimOwed(uint256 agentId, address to) external nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        uint256 amount = owed[agentId];
        if (amount == 0) revert NothingOwed();
        // The wallet first: `_walletOf` is guarded, and a registry whose
        // `ownerOf` reverts must not lock the wallet out too (audit I-3).
        if (msg.sender != _walletOf(agentId) && !_isOwnerOf(msg.sender, agentId)) {
            revert NotOwedClaimer();
        }

        owed[agentId] = 0;
        owedTotal -= amount;
        token.safeTransfer(to, amount);
        emit OwedClaimed(agentId, to, amount);
    }

    /// @inheritdoc ITaskEscrow
    function claimFees() external nonReentrant {
        uint256 amount = feesOwed;
        if (amount == 0) revert NothingOwed();
        feesOwed = 0;
        owedTotal -= amount;
        token.safeTransfer(feeRecipient, amount);
        emit FeesClaimed(feeRecipient, amount);
    }

    // ─────────────────────────── internals ───────────────────────────

    function _settle(uint256 jobId, Job storage job, Outcome outcome, bool recordFeedback)
        internal
    {
        uint128 amount = job.amount;
        uint128 fee = job.fee;
        uint128 paid = amount - fee;
        uint256 workerAgentId = job.workerAgentId;

        // Effects before interactions.
        job.state = JobState.SETTLED;
        lockedTotal -= amount;
        if (outcome == Outcome.UNRESOLVED) _records[workerAgentId].unresolved++;
        else if (recordFeedback) _records[workerAgentId].completed++;

        // Every settlement returns the worker's bond. Never reverts.
        stakeVault.release(jobId);
        _payAgent(jobId, workerAgentId, paid);
        _payFee(jobId, fee);

        emit JobSettled(jobId, paid, fee, outcome);
        if (recordFeedback) _recordFeedback(jobId, workerAgentId, outcome, job);
    }

    function _refund(
        uint256 jobId,
        Job storage job,
        bytes32 reason,
        Outcome outcome,
        bool recordOutcome
    ) internal {
        uint128 amount = job.amount;

        job.state = JobState.REFUNDED;
        lockedTotal -= amount;
        if (recordOutcome) {
            // Only `expireUndelivered` (FAILED) and a lost dispute
            // (DISPUTE_LOST) record an outcome on a refund.
            if (outcome == Outcome.FAILED) _records[job.workerAgentId].failed++;
            else _records[job.workerAgentId].disputesLost++;
        }

        // Not delivering costs the bond, and the client gets it with the
        // refund. Every other refund returns it (a lost dispute judged
        // quality, not absence). Neither call reverts.
        uint256 toClient = amount;
        if (outcome == Outcome.FAILED) {
            uint96 seized = stakeVault.seize(jobId);
            if (seized != 0) {
                toClient += seized;
                emit BondSeized(jobId, job.workerAgentId, seized);
            }
        } else {
            stakeVault.release(jobId);
        }

        _payAgent(jobId, job.clientAgentId, toClient);

        emit JobRefunded(jobId, amount, reason);
        if (recordOutcome) _recordFeedback(jobId, job.workerAgentId, outcome, job);
    }

    /// @dev Move a disputed job's bond out of `disputeBondTotal` to `toAgentId`
    ///      (paid, or held in `owed` — never lost, never blocking the ruling).
    function _settleDisputeBond(uint256 jobId, Job storage job, uint256 toAgentId) internal {
        uint128 bond = job.disputeBond;
        if (bond == 0) return;
        disputeBondTotal -= bond;
        emit DisputeBondSettled(jobId, toAgentId, bond);
        _payAgent(jobId, toAgentId, bond);
    }

    /// @dev Pay an agent, or hold the money for it. Never reverts for a reason
    ///      the recipient controls — that is what used to trap funds.
    ///      `amount` is never zero: a refund returns the whole job (at least
    ///      `minJobAmount`) and a settlement pays `amount - fee`, where
    ///      `_checkParams` keeps the fee below the amount.
    function _payAgent(uint256 jobId, uint256 agentId, uint256 amount) internal {
        address to = _walletOf(agentId);
        if (to == address(0) || !_tryTransfer(to, amount)) {
            owed[agentId] += amount;
            owedTotal += amount;
            emit PaymentDeferred(jobId, agentId, amount);
        }
    }

    function _payFee(uint256 jobId, uint256 fee) internal {
        if (fee == 0) return;
        if (!_tryTransfer(feeRecipient, fee)) {
            feesOwed += fee;
            owedTotal += fee;
            emit FeeDeferred(jobId, fee);
        }
    }

    /// @dev The payout wallet, or zero if the registry cannot say. A low-level
    ///      staticcall with bounded gas that copies at most one word back: a
    ///      `try` would still revert on malformed return data, and an
    ///      unbounded copy could be used to exhaust the caller.
    function _walletOf(uint256 agentId) internal view returns (address wallet) {
        bytes memory data = abi.encodeCall(IIdentityRegistry.getAgentWallet, (agentId));
        address registry = address(identityRegistry);
        uint256 word;
        bool ok;
        assembly ("memory-safe") {
            ok := staticcall(WALLET_LOOKUP_GAS, registry, add(data, 0x20), mload(data), 0, 0x20)
            if and(ok, gt(returndatasize(), 0x1f)) { word := mload(0) }
        }
        // Safe: the high 96 bits are checked to be zero first.
        // forge-lint: disable-next-line(unsafe-typecast)
        if (ok && word >> 160 == 0) wallet = address(uint160(word));
    }

    /// @dev `transfer` that reports failure instead of reverting. Success means
    ///      the call succeeded AND returned `true` or nothing from a contract
    ///      (the SafeERC20 rule).
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

    /// @dev Invariant I8. The Reputation Registry is an upgradeable proxy owned
    ///      by someone else; if it reverts, pauses, or is upgraded badly, a
    ///      direct call here would revert the settlement and TRAP the worker's
    ///      money. Reputation is best-effort. Payment is not.
    ///
    ///      Invariant I9 (v3). Best-effort must not mean caller-optional. A
    ///      `try` forwards at most 63/64 of the gas left, so a caller who picks
    ///      the gas limit could make the registry run out inside the `try` and
    ///      still finish on the 1/64 kept back: job settled, review silently
    ///      dropped. The worker who failed can do that to its own FAILED review
    ///      through `expireUndelivered`. So the call gets a fixed budget, and
    ///      the transaction reverts unless that whole budget is available.
    ///
    ///      Audit M-1. A refused review is not dropped: the registry refuses
    ///      while the agent has made the writer its operator, and an agent
    ///      that switched that on just for its bad reviews would otherwise
    ///      choose its own score. It is kept for `retryFeedback`, and the
    ///      agent cannot hire or be hired until it is written.
    function _recordFeedback(uint256 jobId, uint256 agentId, Outcome outcome, Job storage job)
        internal
    {
        int128 value = outcome == Outcome.SUCCESS ? int128(100) : int128(0);
        // Loaded before the budget check, so the check covers them (audit I-1).
        IReputationRegistry registry = reputationRegistry;
        bytes32 hash = _feedbackHash(jobId, job);
        uint256 budget = _feedbackBudget();
        try registry.giveFeedback{gas: budget}(
            agentId, value, 0, "agentx", "settled", "", "", hash
        ) {
        // recorded
        }
        catch {
            _keepUnwritten(jobId, false, agentId, value);
        }
    }

    /// @dev The client's review, written by `clientWriter` from its own address,
    ///      under the same fixed budget and the same rule (I9): the caller
    ///      must leave gas for it or the transaction reverts.
    function _rateClient(uint256 jobId, uint256 clientAgentId, int128 value, Job storage job)
        internal
    {
        address writer = clientWriter;
        if (writer == address(0)) return;
        bytes32 hash = _feedbackHash(jobId, job);
        uint256 budget = _feedbackBudget();
        try ClientReputationWriter(writer).rate{gas: budget}(clientAgentId, value, hash) {
        // recorded
        }
        catch {
            _keepUnwritten(jobId, true, clientAgentId, value);
        }
    }

    function _keepUnwritten(uint256 jobId, bool client, uint256 agentId, int128 value) internal {
        _unwritten[jobId][client] = UnwrittenReview({agentId: agentId, value: value, pending: true});
        unwrittenReviews[agentId]++;
        emit FeedbackFailed(jobId, agentId);
    }

    /// @inheritdoc ITaskEscrow
    /// @dev Not in a `try`: while the registry still refuses, the whole call
    ///      reverts and the review stays kept. Same fixed budget as at
    ///      settlement (I9), so a caller cannot starve it either.
    function retryFeedback(uint256 jobId, bool client) external nonReentrant {
        UnwrittenReview memory r = _unwritten[jobId][client];
        if (!r.pending) revert NothingToRetry();
        Job storage job = _jobs[jobId];
        bytes32 hash = _feedbackHash(jobId, job);
        delete _unwritten[jobId][client];
        unwrittenReviews[r.agentId]--;
        emit FeedbackRetried(jobId, r.agentId);

        if (client) {
            ClientReputationWriter writer = ClientReputationWriter(clientWriter);
            uint256 budget = _feedbackBudget();
            writer.rate{gas: budget}(r.agentId, r.value, hash);
        } else {
            IReputationRegistry registry = reputationRegistry;
            uint256 budget = _feedbackBudget();
            registry.giveFeedback{gas: budget}(
                r.agentId, r.value, 0, "agentx", "settled", "", "", hash
            );
        }
    }

    function _isOwnerOf(address who, uint256 agentId) internal view returns (bool) {
        try identityRegistry.ownerOf(agentId) returns (address o) {
            return o == who;
        } catch {
            return false;
        }
    }

    /// @dev Audit M-1: see `ReviewUnwritten`.
    function _reviewsWritten(uint256 agentId) internal view {
        if (unwrittenReviews[agentId] != 0) revert ReviewUnwritten(agentId);
    }

    /// @dev Revert unless the whole review budget is available (I9).
    function _feedbackBudget() internal view returns (uint256 budget) {
        budget = config.feedbackGas;
        uint256 needed = budget + budget / 63 + FEEDBACK_RESERVE;
        if (gasleft() < needed) revert InsufficientGasForFeedback(gasleft(), needed);
    }

    /// @dev Binds a review to this exact job, spec, result and amount. Anyone
    ///      can recompute it from public chain state and confirm the feedback
    ///      corresponds to a real settled payment.
    function _feedbackHash(uint256 jobId, Job storage job) internal view returns (bytes32) {
        return keccak256(abi.encode(jobId, job.specHash, job.resultHash, job.amount));
    }

    /// @return workerWallet the worker's payout wallet, known to be non-zero
    function _validateHire(uint256 clientAgentId, uint256 workerAgentId, uint128 amount)
        internal
        view
        returns (address workerWallet)
    {
        if (!configured) revert NotConfigured();
        if (amount == 0) revert ZeroAmount();
        if (amount < config.minJobAmount) revert AmountBelowMinimum(amount, config.minJobAmount);
        // Invariant I6.
        if (clientAgentId == workerAgentId) revert SelfDealing();
        if (identityRegistry.getAgentWallet(clientAgentId) != msg.sender) revert NotAgentWallet();
        workerWallet = identityRegistry.getAgentWallet(workerAgentId);
        // An agent with no payout wallet can be hired but never paid.
        if (workerWallet == address(0)) revert NotHireable();
        if (workerWallet == msg.sender) revert SelfDealing();
        // Two ids, one owner: the v1 hole. Distinct ids were never the point.
        if (identityRegistry.ownerOf(clientAgentId) == identityRegistry.ownerOf(workerAgentId)) {
            revert SameOwner();
        }
        if (!stakeVault.isHireable(workerAgentId)) revert NotHireable();
        _notOperator(workerAgentId);
        _reviewsWritten(clientAgentId);
        _reviewsWritten(workerAgentId);
        // The client-side self-block: a client that made the writer its
        // operator would make every review of itself revert.
        address writer = clientWriter;
        if (writer != address(0) && identityRegistry.isAuthorizedOrOwner(writer, clientAgentId)) {
            revert EscrowIsOperator();
        }
    }

    /// @dev See `EscrowIsOperator`. Checked at hire and again at accept; an
    ///      approval granted later still blocks the registry write, which is
    ///      what `_records` is for.
    function _notOperator(uint256 workerAgentId) internal view {
        if (identityRegistry.isAuthorizedOrOwner(address(this), workerAgentId)) {
            revert EscrowIsOperator();
        }
    }

    /// @dev The share of `amount` the worker locks, never below `minBond`, and
    ///      never more than fits the vault's uint96.
    function _bondOf(uint128 amount) internal view returns (uint128 bond) {
        bond = uint128((uint256(amount) * config.bondBps) / BPS_DENOMINATOR);
        if (bond < config.minBond) bond = config.minBond;
        if (bond > type(uint96).max) bond = type(uint96).max;
    }

    /// @dev The share of `amount` a client posts to dispute, never below
    ///      `minDisputeBond`.
    function _disputeBondOf(uint128 amount) internal view returns (uint128 bond) {
        bond = uint128((uint256(amount) * config.disputeBondBps) / BPS_DENOMINATOR);
        if (bond < config.minDisputeBond) bond = config.minDisputeBond;
    }

    /// @dev The percentage fee, rounded down, but never below `minFee`.
    ///      `_checkParams` keeps `minFee < minJobAmount <= amount`, so the
    ///      worker is always paid something.
    function _feeOf(uint128 amount) internal view returns (uint128 fee) {
        fee = uint128((uint256(amount) * config.protocolFeeBps) / BPS_DENOMINATOR);
        if (fee < config.minFee) fee = config.minFee;
    }

    function _checkParams(Config calldata c, address feeRecipient_) internal pure {
        if (feeRecipient_ == address(0)) revert ZeroAddress();
        if (c.protocolFeeBps > MAX_FEE_BPS) revert FeeTooHigh();
        if (
            c.reviewWindow == 0 || c.reviewWindow > MAX_WINDOW || c.maxAcceptWindow == 0
                || c.maxAcceptWindow > MAX_WINDOW || c.maxWorkWindow == 0
                || c.maxWorkWindow > MAX_WINDOW || c.disputeTimeout == 0
                || c.disputeTimeout > MAX_WINDOW
        ) revert InvalidWindow();
        if (c.minJobAmount == 0 || c.minFee >= c.minJobAmount) revert InvalidFeeFloor();
        if (c.fastPathMax != 0 && c.fastPathMax < c.minJobAmount) revert InvalidFeeFloor();
        if (c.maxJobAmount < c.minJobAmount || c.maxJobAmount < c.fastPathMax) {
            revert InvalidFeeFloor();
        }
        if (c.feedbackGas < MIN_FEEDBACK_GAS || c.feedbackGas > MAX_FEEDBACK_GAS) {
            revert InvalidFeedbackGas();
        }
        if (c.bondBps > BPS_DENOMINATOR || c.disputeBondBps > BPS_DENOMINATOR) {
            revert InvalidBond();
        }
    }

    /// @dev Checked downcast. ERC-8004 ids are uint256; storage packs uint64.
    ///      Reverts rather than wrapping — a wrapped id would silently address
    ///      a different agent.
    function _toU64(uint256 agentId) internal pure returns (uint64) {
        if (agentId > type(uint64).max) revert AgentIdTooLarge(agentId);
        // Safe: bounded on the line above.
        // forge-lint: disable-next-line(unsafe-typecast)
        return uint64(agentId);
    }

    function _onlyClientWallet(Job storage job) internal view {
        if (identityRegistry.getAgentWallet(job.clientAgentId) != msg.sender) {
            revert NotAgentWallet();
        }
    }

    function _onlyWorkerWallet(Job storage job) internal view {
        if (identityRegistry.getAgentWallet(job.workerAgentId) != msg.sender) {
            revert NotAgentWallet();
        }
    }

    // ─────────────────────────── views / admin ───────────────────────────

    /// @inheritdoc ITaskEscrow
    function getJob(uint256 jobId) external view returns (Job memory) {
        return _jobs[jobId];
    }

    /// @inheritdoc ITaskEscrow
    function recordOf(uint256 agentId) external view returns (AgentRecord memory) {
        return _records[agentId];
    }

    /// @inheritdoc ITaskEscrow
    function clientRecordOf(uint256 agentId) external view returns (ClientRecord memory) {
        return _clientRecords[agentId];
    }

    /// @notice Stop NEW jobs. Settlement, refund and every exit stay open, so
    ///         pausing can never trap funds already in escrow. A pauser can
    ///         stop new jobs at once in an incident (v3); the admin, a slow
    ///         multisig on mainnet, is the only one who can resume them.
    function pause() external onlyRole(PAUSER_ROLE) {
        _pause();
    }

    /// @notice Allow new jobs again.
    function unpause() external onlyRole(DEFAULT_ADMIN_ROLE) {
        _unpause();
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
