// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IValidationRegistry} from "./interfaces/IERC8004.sol";
import {ITaskEscrow} from "./interfaces/ITaskEscrow.sol";

/// @dev TaskEscrow's public getter; not part of ITaskEscrow.
interface IEscrowIdentity {
    function identityRegistry() external view returns (address);
}

/// @title SettlementValidator — ERC-8004 validation answered from the escrow
/// @notice A worker's owner asks the ERC-8004 Validation Registry to have one
///         of its AGENTX jobs validated; anyone may then call `respond`, and
///         the answer is read from TaskEscrow's own state — no key, no
///         opinion, nothing an operator could shade. Any reader can recompute
///         it from the chain:
///
///         - 100: the job SETTLED with a result on chain and was never disputed
///           (the client approved it, or let the review window pass);
///         - 0:   the job was REFUNDED after a result was delivered and disputed
///           — only an arbiter upholding the dispute ends that way.
///
///         Anything else is refused, not guessed: an open job, a direct-pay
///         job (no result on chain), a disputed job that settled (a ruling for
///         the worker and an unanswered timeout look the same), and a refund
///         without a result (a cancel, an unaccepted offer and a no-show look
///         the same). The worker's ERC-8004 reputation already records every
///         outcome the escrow can tell apart.
///
///         The request hash commits to the job and a salt the worker picks, so
///         a request cannot be answered with some other job. A copy of the
///         hash registered for another agent is refused, and the worker asks
///         again with a new salt. Each job is answered once.
///
///         What it is not: an unbiased record. The worker chooses which jobs to
///         have validated, so a count of validations says nothing about the
///         jobs it left out — read the escrow's reviews (or `recordOf`) for
///         that. A validation proves one specific delivery, on chain.
contract SettlementValidator {
    /// @notice The `tag` every response carries.
    string public constant TAG = "agentx-settlement";

    ITaskEscrow public immutable escrow;
    IValidationRegistry public immutable validationRegistry;

    /// @notice Jobs already answered.
    mapping(uint256 jobId => bool) public answered;

    event Validated(
        uint256 indexed jobId,
        uint256 indexed workerAgentId,
        bytes32 indexed requestHash,
        uint8 response
    );

    error ZeroAddress();
    error IdentityMismatch();
    error AlreadyAnswered(uint256 jobId);
    error NoSuchRequest(bytes32 requestHash);
    error WrongValidator(address named);
    error NotTheWorker(uint256 requestAgentId, uint256 workerAgentId);
    error NotAttributable(uint256 jobId, ITaskEscrow.JobState state);

    /// @param escrow_ the TaskEscrow whose jobs are validated
    /// @param validationRegistry_ the ERC-8004 Validation Registry; it must
    ///        name the same identity registry as the escrow, or agent ids
    ///        would not mean the same agent
    constructor(ITaskEscrow escrow_, IValidationRegistry validationRegistry_) {
        if (address(escrow_) == address(0) || address(validationRegistry_) == address(0)) {
            revert ZeroAddress();
        }
        if (
            validationRegistry_.getIdentityRegistry()
                != IEscrowIdentity(address(escrow_)).identityRegistry()
        ) {
            revert IdentityMismatch();
        }
        escrow = escrow_;
        validationRegistry = validationRegistry_;
    }

    /// @notice The hash a worker's owner passes to `validationRequest`, naming
    ///         this contract as the validator.
    /// @param jobId the escrow job
    /// @param salt any value; pick a fresh one if a request is ever squatted
    function requestHashOf(uint256 jobId, bytes32 salt) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), jobId, salt));
    }

    /// @notice What `respond` would answer for a job, without writing.
    /// @return attributable false if the escrow's state cannot say
    /// @return response 0-100, meaningful only when attributable
    function verdictOf(uint256 jobId) public view returns (bool attributable, uint8 response) {
        ITaskEscrow.Job memory job = escrow.getJob(jobId);
        if (job.resultHash == bytes32(0)) return (false, 0);
        if (job.state == ITaskEscrow.JobState.SETTLED && job.disputeDeadline == 0) {
            return (true, 100);
        }
        if (job.state == ITaskEscrow.JobState.REFUNDED && job.disputeDeadline != 0) {
            return (true, 0);
        }
        return (false, 0);
    }

    /// @notice Answer a validation request for a job. Anyone may call.
    /// @param jobId the escrow job the request is for
    /// @param salt the salt in the request hash
    function respond(uint256 jobId, bytes32 salt) external returns (uint8 response) {
        if (answered[jobId]) revert AlreadyAnswered(jobId);
        bytes32 requestHash = requestHashOf(jobId, salt);

        // Only the first two words are needed — validator and agent id, both
        // static — so only they are decoded. The registry reverts "unknown"
        // for a hash nobody requested.
        (bool ok, bytes memory ret) = address(validationRegistry)
            .staticcall(abi.encodeCall(IValidationRegistry.getValidationStatus, (requestHash)));
        if (!ok || ret.length < 64) revert NoSuchRequest(requestHash);
        (address named, uint256 requestAgentId) = abi.decode(ret, (address, uint256));
        if (named != address(this)) revert WrongValidator(named);

        ITaskEscrow.Job memory job = escrow.getJob(jobId);
        if (requestAgentId != job.workerAgentId) {
            revert NotTheWorker(requestAgentId, job.workerAgentId);
        }
        bool attributable;
        (attributable, response) = verdictOf(jobId);
        if (!attributable) revert NotAttributable(jobId, job.state);

        answered[jobId] = true;
        emit Validated(jobId, job.workerAgentId, requestHash, response);
        // The same binding as the escrow's feedback hash, so the two records
        // of one job can be matched by anyone.
        bytes32 responseHash =
            keccak256(abi.encode(jobId, job.specHash, job.resultHash, job.amount));
        validationRegistry.validationResponse(requestHash, response, "", responseHash, TAG);
    }
}
