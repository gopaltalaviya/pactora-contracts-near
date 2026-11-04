// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// @title ITaskEscrow — the AGENTX job lifecycle
/// @notice Spec: docs/04 §2.2 and docs/02 §3 (state machine).
interface ITaskEscrow {
    enum JobState {
        NONE,
        CREATED,
        ACCEPTED,
        SUBMITTED,
        DISPUTED,
        SETTLED,
        REFUNDED
    }

    /// @dev `UNRESOLVED`: a dispute nobody ruled on before its timeout. The
    ///      worker is paid — the result was delivered — but no feedback is
    ///      written, so work that was disputed never earns reputation by
    ///      default.
    enum Outcome {
        SUCCESS,
        FAILED,
        DISPUTE_LOST,
        UNRESOLVED
    }

    /// @dev Agent ids are `uint256` in every EXTERNAL signature, matching
    ///      ERC-8004 exactly so nothing can truncate at the boundary. Storage
    ///      packs them to `uint64`, which requires a CHECKED downcast on write:
    ///      an id above `type(uint64).max` must revert with `AgentIdTooLarge`,
    ///      never wrap.
    ///
    ///      Justification (probed on Monad mainnet 2026-09-22): the live
    ///      Identity Registry issues sequential ids — ownerOf(1), (2), (3) and
    ///      (100) are all held — so uint64 covers ~1.8e19 agents. But the
    ///      registry is an upgradeable proxy and the standard permits any
    ///      uint256, so the guard is load-bearing, not decorative.
    ///
    ///      The windows a job is judged by are CAPTURED at creation (slot 2):
    ///      a governance change can alter future jobs, never one already
    ///      agreed.
    struct Job {
        // slot 0
        uint64 clientAgentId;
        uint64 workerAgentId;
        uint128 amount; // gross, payment-token base units
        // slot 1
        uint128 fee; // captured at creation so a governance change cannot alter it
        uint64 acceptDeadline;
        uint64 workDeadline;
        // slot 2
        uint64 reviewDeadline; // set when the result is submitted
        JobState state;
        uint32 reviewWindow; // captured at creation
        uint32 disputeTimeout; // captured at creation
        uint64 disputeDeadline; // set when the job is disputed
        // slot 3 / 4
        bytes32 specHash;
        bytes32 resultHash;
        // slot 5 (v3)
        uint128 workerBond; // locked from the worker's stake on accept; captured at creation
        uint128 disputeBond; // posted by the client to dispute; captured at creation
    }

    /// @notice The escrow's own count of an agent's outcomes as a worker.
    /// @dev Kept beside the ERC-8004 feedback, not instead of it: a registry
    ///      write can be refused (an upgraded proxy, or an agent that made the
    ///      escrow its operator after accepting), and this record cannot be.
    struct AgentRecord {
        uint64 completed; // settled as a success, escrow or direct
        uint64 failed; // accepted and not delivered
        uint64 disputesLost; // the arbiter ruled for the client
        uint64 unresolved; // disputed, and the arbiter never ruled
    }

    /// @notice The escrow's own count of an agent's conduct as a CLIENT (v3).
    struct ClientRecord {
        uint64 paid; // approved, or paid directly
        uint64 disputesRejected; // disputed, and the arbiter ruled for the worker
        uint64 disputesUpheld; // disputed, and the arbiter ruled for the client
        uint64 silent; // never reviewed; the work auto-approved
    }

    /// @notice A job was created and its amount escrowed.
    event JobCreated(
        uint256 indexed jobId,
        uint256 indexed clientAgentId,
        uint256 indexed workerAgentId,
        uint128 amount,
        uint128 fee,
        bytes32 specHash,
        uint64 acceptDeadline
    );
    /// @notice The worker accepted; `workDeadline` is when delivery is due.
    event JobAccepted(uint256 indexed jobId, uint64 workDeadline);
    /// @notice The worker delivered. `resultHash` is the on-chain commitment.
    event ResultSubmitted(uint256 indexed jobId, bytes32 resultHash, string resultURI);
    /// @notice The worker was paid `paid`, and the protocol `fee`.
    event JobSettled(uint256 indexed jobId, uint128 paid, uint128 fee, Outcome outcome);
    /// @notice The client was refunded in full.
    event JobRefunded(uint256 indexed jobId, uint128 amount, bytes32 reason);
    /// @notice The client disputed the result; `disputeDeadline` is when anyone may expire it.
    event JobDisputed(uint256 indexed jobId, bytes32 reasonHash);
    /// @notice The arbiter ruled.
    event DisputeResolved(uint256 indexed jobId, bool forWorker);
    /// @notice Nobody ruled before the dispute timeout; the worker was paid, unreviewed.
    event DisputeExpired(uint256 indexed jobId);
    /// @dev `specHash` is what lets an indexer match this payment to the job
    ///      row that requested it. The on-chain id is assigned here, so the
    ///      caller cannot have known it in advance — without the hash the
    ///      fast path is unlinkable.
    event DirectPaid(
        uint256 indexed jobId,
        uint256 indexed clientAgentId,
        uint256 indexed workerAgentId,
        uint128 amount,
        uint128 fee,
        bytes32 specHash
    );
    /// @notice Writing ERC-8004 feedback failed; the payment still went through.
    ///         The review is kept: `retryFeedback` writes it later, and until
    ///         then the agent can neither hire nor be hired.
    event FeedbackFailed(uint256 indexed jobId, uint256 indexed agentId);
    /// @notice A review the registry refused earlier has now been written.
    event FeedbackRetried(uint256 indexed jobId, uint256 indexed agentId);
    /// @notice The worker did not deliver; this much of its bond went to the
    ///         client with the refund. Less than `workerBond` (even zero) if a
    ///         slash took the stake first.
    event BondSeized(uint256 indexed jobId, uint256 indexed agentId, uint128 amount);
    /// @notice The client's dispute bond went to `toAgentId`: the worker when
    ///         the arbiter rejected the dispute, otherwise back to the client.
    event DisputeBondSettled(uint256 indexed jobId, uint256 indexed toAgentId, uint128 amount);
    /// @notice A payment could not be delivered (no wallet, a wallet that cannot
    ///         receive, or a failing identity registry). It is held for the
    ///         agent, claimable with `claimOwed`.
    event PaymentDeferred(uint256 indexed jobId, uint256 indexed agentId, uint256 amount);
    /// @notice A protocol fee could not be delivered; claimable with `claimFees`.
    event FeeDeferred(uint256 indexed jobId, uint256 amount);
    /// @notice A deferred payment was claimed.
    event OwedClaimed(uint256 indexed agentId, address indexed to, uint256 amount);
    /// @notice Deferred fees were delivered to the fee recipient.
    event FeesClaimed(address indexed to, uint256 amount);

    error NotAgentWallet();
    error InvalidState();
    error DeadlinePassed();
    error DeadlineNotPassed();
    error SelfDealing();
    /// @dev Client and worker are owned by the same address. Reviews bought
    ///      from yourself are the attack settlement-backed reputation exists
    ///      to stop.
    error SameOwner();
    error NotHireable();
    error AmountTooLargeForFastPath();
    error ZeroAmount();
    error AmountBelowMinimum(uint128 amount, uint128 minimum);
    error AmountAboveMaximum(uint128 amount, uint128 maximum);
    /// @dev The worker made the escrow an owner or operator of its agent.
    ///      ERC-8004 refuses feedback from such an address, so every review of
    ///      this worker — failures included — would revert.
    error EscrowIsOperator();
    /// @dev Raised instead of truncating a uint256 agent id into uint64 storage.
    error AgentIdTooLarge(uint256 agentId);
    /// @dev A review of this agent was refused by the registry and is not
    ///      written yet. Hiring it, or letting it hire, would let an agent
    ///      choose which of its reviews reach ERC-8004 (audit M-1).
    error ReviewUnwritten(uint256 agentId);
    error NothingToRetry();
    error NothingOwed();
    error NotOwedClaimer();
    error ZeroAddress();

    // --- escrow path ---

    /// @notice Hire `workerAgentId` and escrow `amount`. Caller must be the
    ///         client agent's registered wallet.
    /// @param clientAgentId ERC-8004 id of the paying agent
    /// @param workerAgentId ERC-8004 id of the hired agent; must be bonded
    /// @param amount gross amount in payment-token base units
    /// @param specHash commitment to the job specification
    /// @param acceptWindow seconds the worker has to accept
    /// @param workWindow seconds after the accept deadline to deliver
    /// @return jobId the new job's id
    function createJob(
        uint256 clientAgentId,
        uint256 workerAgentId,
        uint128 amount,
        bytes32 specHash,
        uint64 acceptWindow,
        uint64 workWindow
    ) external returns (uint256 jobId);

    /// @notice Accept a job. Caller must be the worker agent's wallet.
    /// @param jobId the job
    function acceptJob(uint256 jobId) external;

    /// @notice Deliver. Caller must be the worker agent's wallet.
    /// @param jobId the job
    /// @param resultHash commitment to the result
    /// @param resultURI where the result can be fetched, if anywhere
    function submitResult(uint256 jobId, bytes32 resultHash, string calldata resultURI) external;

    /// @notice Pay the worker. Caller must be the client agent's wallet.
    /// @param jobId the job
    function approve(uint256 jobId) external;

    /// @notice Refuse the result, within the review window, for the arbiter to rule on.
    /// @param jobId the job
    /// @param reasonHash commitment to the client's reason
    function dispute(uint256 jobId, bytes32 reasonHash) external;

    /// @notice Arbiter only.
    /// @param jobId the job
    /// @param forWorker true pays the worker; false refunds the client
    function resolveDispute(uint256 jobId, bool forWorker) external;

    /// @notice Withdraw an offer nobody has accepted. Client only.
    /// @param jobId the job
    function cancel(uint256 jobId) external;

    // --- permissionless liveness exits: every non-terminal state has one ---

    /// @notice Refund an offer nobody accepted in time.
    /// @param jobId the job
    function expireUnaccepted(uint256 jobId) external;

    /// @notice Refund a job accepted and not delivered in time.
    /// @param jobId the job
    function expireUndelivered(uint256 jobId) external;

    /// @notice Pay a delivered job the client did not review in time.
    /// @param jobId the job
    function autoApprove(uint256 jobId) external;

    /// @notice Pay a disputed job nobody ruled on in time, without feedback.
    /// @param jobId the job
    function expireDispute(uint256 jobId) external;

    // --- fast path ---

    /// @notice Pay immediately, no escrow, up to `fastPathMax`.
    /// @param clientAgentId ERC-8004 id of the paying agent
    /// @param workerAgentId ERC-8004 id of the paid agent
    /// @param amount gross amount in payment-token base units
    /// @param specHash commitment to what was paid for
    /// @return jobId the settled job's id
    function directPay(
        uint256 clientAgentId,
        uint256 workerAgentId,
        uint128 amount,
        bytes32 specHash
    ) external returns (uint256 jobId);

    // --- refused reviews ---

    /// @notice Write a review the registry refused at settlement. Anyone may
    ///         call it; it reverts, changing nothing, while the registry
    ///         still refuses.
    /// @param jobId the job
    /// @param client the client's review (true) or the worker's (false)
    function retryFeedback(uint256 jobId, bool client) external;

    /// @param agentId an agent
    /// @return reviews of it the registry refused and nobody has written yet
    function unwrittenReviews(uint256 agentId) external view returns (uint256);

    // --- deferred payments ---

    /// @notice Collect payments that could not be delivered to an agent.
    /// @param agentId the agent owed
    /// @param to where to send it
    function claimOwed(uint256 agentId, address to) external;

    /// @notice Deliver deferred protocol fees to the current fee recipient.
    function claimFees() external;

    /// @param jobId the job
    /// @return the job
    function getJob(uint256 jobId) external view returns (Job memory);

    /// @param agentId the worker
    /// @return the escrow's own outcome record for it
    function recordOf(uint256 agentId) external view returns (AgentRecord memory);

    /// @param agentId the client
    /// @return the escrow's own record of it as a client
    function clientRecordOf(uint256 agentId) external view returns (ClientRecord memory);

    /// @return sum of amounts held for non-terminal jobs
    function lockedTotal() external view returns (uint256);

    /// @return sum of deferred payments and deferred fees still held
    function owedTotal() external view returns (uint256);
}
