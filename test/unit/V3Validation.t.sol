// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {SettlementValidator} from "../../src/SettlementValidator.sol";
import {IValidationRegistry} from "../../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {UpstreamValidationRegistry} from "../vendor/UpstreamValidationRegistry.sol";

/// @dev A registry that names the right identity but answers a status lookup
///      with success and no data — an upgraded or broken proxy.
contract MuteValidationRegistry {
    address internal immutable identity;

    constructor(address identity_) {
        identity = identity_;
    }

    function getIdentityRegistry() external view returns (address) {
        return identity;
    }

    fallback() external {}
}

/// @dev v3: ERC-8004 validation answered from the escrow's own state, against
///      the upstream registry's logic (test/vendor). Run against the deployed
///      registry itself in test/fork/V3Live.fork.t.sol.
contract V3ValidationTest is BaseTest {
    UpstreamValidationRegistry internal registry;
    SettlementValidator internal validator;
    bytes32 internal constant SALT = keccak256("salt");

    function setUp() public override {
        super.setUp();
        registry = new UpstreamValidationRegistry(address(identity));
        // The mock identity has no ERC-721 approvals; say so to the registry,
        // which asks. Only the owner may request.
        vm.mockCall(
            address(identity),
            abi.encodeWithSignature("getApproved(uint256)"),
            abi.encode(address(0))
        );
        vm.mockCall(
            address(identity),
            abi.encodeWithSignature("isApprovedForAll(address,address)"),
            abi.encode(false)
        );
        validator = new SettlementValidator(escrow, IValidationRegistry(address(registry)));
    }

    function _request(uint256 jobId, bytes32 salt) internal returns (bytes32 h) {
        h = validator.requestHashOf(jobId, salt);
        vm.prank(workerOwner);
        registry.validationRequest(address(validator), workerId, "https://api/v1/jobs/x", h);
    }

    function _approved() internal returns (uint256 jobId) {
        jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.approve(jobId);
    }

    function _disputed() internal returns (uint256 jobId) {
        jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
    }

    // ── what it answers ───────────────────────────────────────────────────

    function test_anApprovedJobValidatesAt100() public {
        uint256 jobId = _approved();
        bytes32 h = _request(jobId, SALT);
        vm.prank(stranger); // anyone may ask for the answer
        assertEq(validator.respond(jobId, SALT), 100);

        (address v, uint256 agentId, uint8 response, bytes32 responseHash, string memory tag,) =
            registry.getValidationStatus(h);
        assertEq(v, address(validator));
        assertEq(agentId, workerId);
        assertEq(response, 100);
        assertEq(tag, "agentx-settlement");
        ITaskEscrow.Job memory job = escrow.getJob(jobId);
        assertEq(
            responseHash,
            keccak256(abi.encode(jobId, job.specHash, job.resultHash, job.amount)),
            "the same binding as the escrow's feedback hash"
        );
        address[] memory only = new address[](1);
        only[0] = address(validator);
        (uint64 count, uint8 avg) = registry.getSummary(workerId, only, "agentx-settlement");
        assertEq(count, 1);
        assertEq(avg, 100);
    }

    function test_aSilentClientsAutoApprovedJobValidatesAt100() public {
        uint256 jobId = _submittedJob();
        vm.warp(block.timestamp + REVIEW_WINDOW + 1);
        escrow.autoApprove(jobId);
        _request(jobId, SALT);
        assertEq(validator.respond(jobId, SALT), 100);
    }

    function test_anUpheldDisputeValidatesAt0() public {
        uint256 jobId = _disputed();
        vm.prank(admin);
        escrow.resolveDispute(jobId, false);
        _request(jobId, SALT);
        assertEq(validator.respond(jobId, SALT), 0);
    }

    // ── what it refuses rather than guesses ───────────────────────────────

    function test_aDisputeTheWorkerWonIsNotAttributable() public {
        // Indistinguishable on chain from a dispute nobody ruled on.
        uint256 jobId = _disputed();
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        _request(jobId, SALT);
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NotAttributable.selector, jobId, ITaskEscrow.JobState.SETTLED
            )
        );
        validator.respond(jobId, SALT);
    }

    function test_anExpiredDisputeIsNotAttributable() public {
        uint256 jobId = _disputed();
        vm.warp(block.timestamp + DISPUTE_TIMEOUT + 1);
        escrow.expireDispute(jobId);
        _request(jobId, SALT);
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NotAttributable.selector, jobId, ITaskEscrow.JobState.SETTLED
            )
        );
        validator.respond(jobId, SALT);
    }

    function test_anOpenJobIsNotAttributable() public {
        uint256 jobId = _submittedJob();
        _request(jobId, SALT);
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NotAttributable.selector, jobId, ITaskEscrow.JobState.SUBMITTED
            )
        );
        validator.respond(jobId, SALT);
    }

    function test_aRefundWithoutAResultIsNotAttributable() public {
        // A cancel, an unaccepted offer and a no-show all end like this.
        uint256 jobId = _acceptedJob();
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        _request(jobId, SALT);
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NotAttributable.selector, jobId, ITaskEscrow.JobState.REFUNDED
            )
        );
        validator.respond(jobId, SALT);
    }

    function test_aDirectPaymentIsNotAttributable() public {
        // Settled, but the result never reaches the chain.
        vm.prank(clientWallet);
        uint256 jobId = escrow.directPay(clientId, workerId, MICRO, keccak256("m"));
        _request(jobId, SALT);
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NotAttributable.selector, jobId, ITaskEscrow.JobState.SETTLED
            )
        );
        validator.respond(jobId, SALT);
    }

    // ── binding: the request, the job and the worker ──────────────────────

    function test_eachJobIsAnsweredOnce() public {
        uint256 jobId = _approved();
        _request(jobId, SALT);
        validator.respond(jobId, SALT);
        _request(jobId, keccak256("again"));
        vm.expectRevert(abi.encodeWithSelector(SettlementValidator.AlreadyAnswered.selector, jobId));
        validator.respond(jobId, keccak256("again"));
    }

    function test_aRequestCannotBeAnsweredWithAnotherJob() public {
        uint256 good = _approved();
        uint256 bad = _disputed();
        vm.prank(admin);
        escrow.resolveDispute(bad, false);
        bytes32 asked = _request(good, SALT);
        // The griefer's version: the worker's salt, the bad job.
        bytes32 wouldBe = validator.requestHashOf(bad, SALT);
        assertTrue(asked != wouldBe);
        vm.expectRevert(abi.encodeWithSelector(SettlementValidator.NoSuchRequest.selector, wouldBe));
        validator.respond(bad, SALT);
    }

    function test_aSquattedRequestIsRefusedAndTheWorkerAsksAgain() public {
        uint256 jobId = _approved();
        // Someone else's agent registers the worker's hash first.
        bytes32 h = validator.requestHashOf(jobId, SALT);
        vm.prank(clientOwner);
        registry.validationRequest(address(validator), clientId, "", h);
        vm.expectRevert(
            abi.encodeWithSelector(SettlementValidator.NotTheWorker.selector, clientId, workerId)
        );
        validator.respond(jobId, SALT);
        assertFalse(validator.answered(jobId), "the job is still open to its worker");

        bytes32 fresh = keccak256("fresh");
        _request(jobId, fresh);
        assertEq(validator.respond(jobId, fresh), 100);
    }

    function test_aRequestNamingAnotherValidatorIsRefused() public {
        uint256 jobId = _approved();
        bytes32 h = validator.requestHashOf(jobId, SALT);
        vm.prank(workerOwner);
        registry.validationRequest(stranger, workerId, "", h);
        vm.expectRevert(
            abi.encodeWithSelector(SettlementValidator.WrongValidator.selector, stranger)
        );
        validator.respond(jobId, SALT);
    }

    function test_noRequestNoAnswer() public {
        uint256 jobId = _approved();
        bytes32 h = validator.requestHashOf(jobId, SALT);
        vm.expectRevert(abi.encodeWithSelector(SettlementValidator.NoSuchRequest.selector, h));
        validator.respond(jobId, SALT);
    }

    function test_onlyTheWorkersOwnerCanAsk() public {
        // Upstream's rule, and what makes the request's agent id trustworthy.
        uint256 jobId = _approved();
        bytes32 h = validator.requestHashOf(jobId, SALT);
        vm.prank(stranger);
        vm.expectRevert("Not authorized");
        registry.validationRequest(address(validator), workerId, "", h);
    }

    function test_theHashIsBoundToChainAndValidator() public {
        bytes32 here = validator.requestHashOf(1, SALT);
        SettlementValidator other =
            new SettlementValidator(escrow, IValidationRegistry(address(registry)));
        assertTrue(other.requestHashOf(1, SALT) != here, "another validator");
        vm.chainId(143);
        assertTrue(validator.requestHashOf(1, SALT) != here, "another chain");
    }

    function test_verdictOfMatchesRespond() public {
        uint256 jobId = _approved();
        (bool ok, uint8 r) = validator.verdictOf(jobId);
        assertTrue(ok);
        assertEq(r, 100);
        (ok,) = validator.verdictOf(_submittedJob());
        assertFalse(ok);
    }

    // ── construction ──────────────────────────────────────────────────────

    function test_theRegistryMustShareTheEscrowsIdentity() public {
        UpstreamValidationRegistry elsewhere = new UpstreamValidationRegistry(makeAddr("other"));
        vm.expectRevert(SettlementValidator.IdentityMismatch.selector);
        new SettlementValidator(escrow, IValidationRegistry(address(elsewhere)));
    }

    function test_zeroAddressesAreRefused() public {
        vm.expectRevert(SettlementValidator.ZeroAddress.selector);
        new SettlementValidator(ITaskEscrow(address(0)), IValidationRegistry(address(registry)));
        vm.expectRevert(SettlementValidator.ZeroAddress.selector);
        new SettlementValidator(escrow, IValidationRegistry(address(0)));
    }

    function test_aMalformedStatusIsNoSuchRequest() public {
        MuteValidationRegistry mute = new MuteValidationRegistry(address(identity));
        SettlementValidator v = new SettlementValidator(escrow, IValidationRegistry(address(mute)));
        uint256 jobId = _approved();
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementValidator.NoSuchRequest.selector, v.requestHashOf(jobId, SALT)
            )
        );
        v.respond(jobId, SALT);
    }
}
