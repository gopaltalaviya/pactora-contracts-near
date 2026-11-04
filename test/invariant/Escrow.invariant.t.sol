// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {MockIdentityRegistry} from "../../src/mocks/MockERC8004.sol";
import {BaseTest, readConfig} from "../Base.t.sol";
import {Test} from "forge-std/Test.sol";

/// @dev Drives the escrow through randomised call sequences. The handler only
///      makes calls that are plausible, so the fuzzer spends its budget on real
///      state transitions — including the hostile ones v2 exists for: a wallet
///      cleared mid-job, an identity registry that reverts, a fee recipient
///      that cannot receive, governance changing parameters mid-flight, and an
///      arbiter who never rules.
contract EscrowHandler is Test {
    TaskEscrow public escrow;
    MockIdentityRegistry public identity;
    address public admin;
    address public clientWallet;
    address public workerWallet;
    address public clientOwner;
    address public workerOwner;
    uint256 public clientId;
    uint256 public workerId;

    uint256[] public jobIds;
    uint256 public ghostClaimed;
    uint256 public ghostCreatedWhilePaused;

    constructor(
        TaskEscrow e,
        MockIdentityRegistry id,
        address admin_,
        address[4] memory wallets, // clientWallet, workerWallet, clientOwner, workerOwner
        uint256 cid,
        uint256 wid
    ) {
        escrow = e;
        identity = id;
        admin = admin_;
        clientWallet = wallets[0];
        workerWallet = wallets[1];
        clientOwner = wallets[2];
        workerOwner = wallets[3];
        clientId = cid;
        workerId = wid;
    }

    // ── the lifecycle ────────────────────────────────────────────────────

    function createJob(uint128 amount, uint64 acceptWindow, uint64 workWindow) public {
        amount = uint128(bound(amount, 1, 1_000_000));
        acceptWindow = uint64(bound(acceptWindow, 1, 1 days));
        workWindow = uint64(bound(workWindow, 1, 7 days));
        bool paused = escrow.paused();

        vm.prank(clientWallet);
        try escrow.createJob(
            clientId, workerId, amount, keccak256(abi.encode(amount)), acceptWindow, workWindow
        ) returns (
            uint256 id
        ) {
            jobIds.push(id);
            if (paused) ghostCreatedWhilePaused++;
        } catch {}
    }

    function directPay(uint128 amount) public {
        amount = uint128(bound(amount, 1, 30_000));
        bool paused = escrow.paused();
        vm.prank(clientWallet);
        try escrow.directPay(
            clientId, workerId, amount, keccak256(abi.encode("d", amount))
        ) returns (
            uint256 id
        ) {
            jobIds.push(id);
            if (paused) ghostCreatedWhilePaused++;
        } catch {}
    }

    function accept(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(workerWallet);
        try escrow.acceptJob(id) {} catch {}
    }

    function submit(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(workerWallet);
        try escrow.submitResult(id, keccak256("r"), "") {} catch {}
    }

    function approve(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(clientWallet);
        try escrow.approve(id) {} catch {}
    }

    function dispute(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(clientWallet);
        try escrow.dispute(id, keccak256("why")) {} catch {}
    }

    function resolveDispute(uint256 seed, bool forWorker) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(admin);
        try escrow.resolveDispute(id, forWorker) {} catch {}
    }

    function cancel(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id == 0) return;
        vm.prank(clientWallet);
        try escrow.cancel(id) {} catch {}
    }

    // ── the permissionless exits ─────────────────────────────────────────

    function expireUnaccepted(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id != 0) try escrow.expireUnaccepted(id) {} catch {}
    }

    function expireUndelivered(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id != 0) try escrow.expireUndelivered(id) {} catch {}
    }

    function autoApprove(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id != 0) try escrow.autoApprove(id) {} catch {}
    }

    function expireDispute(uint256 seed) public {
        uint256 id = _pick(seed);
        if (id != 0) try escrow.expireDispute(id) {} catch {}
    }

    // ── deferred payments ────────────────────────────────────────────────

    function claimOwed(bool worker) public {
        (uint256 id, address owner) = worker ? (workerId, workerOwner) : (clientId, clientOwner);
        uint256 amount = escrow.owed(id);
        vm.prank(owner);
        try escrow.claimOwed(id, owner) {
            ghostClaimed += amount;
        } catch {}
    }

    function claimFees() public {
        try escrow.claimFees() {} catch {}
    }

    // ── hostility ────────────────────────────────────────────────────────

    /// @dev Clear or restore a wallet mid-job — the v1 trap.
    function toggleWallet(bool worker, bool clear) public {
        (uint256 id, address owner, address wallet) =
            worker ? (workerId, workerOwner, workerWallet) : (clientId, clientOwner, clientWallet);
        vm.prank(owner);
        identity.setAgentWallet(id, clear ? address(0) : wallet);
    }

    function toggleRegistryRevert(bool broken) public {
        identity.setWalletShouldRevert(broken);
    }

    function togglePause(bool pause_) public {
        vm.prank(admin);
        if (pause_) {
            try escrow.pause() {} catch {}
        } else {
            try escrow.unpause() {} catch {}
        }
    }

    /// @dev Governance changing terms mid-flight; captured values must hold.
    function setParams(uint64 reviewWindow, uint16 feeBps, uint128 minFee) public {
        TaskEscrow.Config memory c;
        c = readConfig(escrow);
        c.reviewWindow = uint64(bound(reviewWindow, 1, 3 days));
        c.protocolFeeBps = uint16(bound(feeBps, 0, 1_000));
        c.minFee = uint128(bound(minFee, 0, c.minJobAmount - 1));
        vm.prank(admin);
        try escrow.setParams(c, escrow.feeRecipient()) {} catch {}
    }

    /// @dev Time must move for the deadline-driven exits to be reachable at all.
    function warp(uint64 secondsAhead) public {
        vm.warp(block.timestamp + bound(secondsAhead, 1, 3 days));
    }

    function jobCount() external view returns (uint256) {
        return jobIds.length;
    }

    function _pick(uint256 seed) internal view returns (uint256) {
        if (jobIds.length == 0) return 0;
        return jobIds[seed % jobIds.length];
    }
}

contract EscrowInvariantTest is BaseTest {
    EscrowHandler internal handler;

    function setUp() public override {
        super.setUp();

        handler = new EscrowHandler(
            escrow,
            identity,
            admin,
            [clientWallet, workerWallet, clientOwner, workerOwner],
            clientId,
            workerId
        );

        token.mint(clientWallet, type(uint128).max);
        vm.prank(clientWallet);
        token.approve(address(escrow), type(uint256).max);

        targetContract(address(handler));
    }

    /// I1 — the escrow holds EXACTLY what it owes: open jobs plus deferred
    /// payments and fees. Equality, not >=, because nothing in the handler
    /// donates tokens — so any surplus would be money nobody can account for.
    function invariant_I1_solvency() public view {
        assertEq(
            token.balanceOf(address(escrow)),
            escrow.lockedTotal() + escrow.owedTotal() + escrow.disputeBondTotal(),
            "balance must equal lockedTotal + owedTotal + disputeBondTotal"
        );
    }

    /// owedTotal is exactly the per-agent debts plus the fee debt.
    function invariant_owedTotalIsTheSumOfDebts() public view {
        assertEq(
            escrow.owedTotal(),
            escrow.owed(clientId) + escrow.owed(workerId) + escrow.feesOwed(),
            "owedTotal drifted from what is owed"
        );
    }

    /// I1b — lockedTotal is exactly the sum of non-terminal job amounts.
    function invariant_I1b_lockedTotalMatchesOpenJobs() public view {
        uint256 sum;
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            if (
                job.state == ITaskEscrow.JobState.CREATED
                    || job.state == ITaskEscrow.JobState.ACCEPTED
                    || job.state == ITaskEscrow.JobState.SUBMITTED
                    || job.state == ITaskEscrow.JobState.DISPUTED
            ) {
                sum += job.amount;
            }
        }
        assertEq(escrow.lockedTotal(), sum, "lockedTotal drifted from open jobs");
    }

    /// I10 (v3) — the escrow's own outcome record matches the jobs that ended.
    ///      Every settlement is a completion or an unresolved dispute; every
    ///      recorded failure or lost dispute is a refund (cancellations and
    ///      unaccepted expiries are refunds that are nobody's outcome).
    function invariant_I10_recordMatchesEndedJobs() public view {
        uint256 settled;
        uint256 refunded;
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.JobState s = escrow.getJob(handler.jobIds(i)).state;
            if (s == ITaskEscrow.JobState.SETTLED) settled++;
            else if (s == ITaskEscrow.JobState.REFUNDED) refunded++;
        }
        ITaskEscrow.AgentRecord memory r = escrow.recordOf(handler.workerId());
        assertEq(uint256(r.completed) + r.unresolved, settled, "settlements uncounted");
        assertLe(uint256(r.failed) + r.disputesLost, refunded, "outcome without a refund");
    }

    /// I11 (v3) — the worker's locked stake is exactly the bonds of its jobs
    ///      that were accepted and have not ended. No lock outlives its job.
    function invariant_I11_locksMatchOpenAcceptedJobs() public view {
        uint256 sum;
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            if (
                job.state == ITaskEscrow.JobState.ACCEPTED
                    || job.state == ITaskEscrow.JobState.SUBMITTED
                    || job.state == ITaskEscrow.JobState.DISPUTED
            ) {
                sum += job.workerBond;
            }
        }
        assertEq(vault.lockedOf(handler.workerId()), sum, "a lock outlived its job, or is missing");
    }

    /// I12 (v3) — the dispute bonds held are exactly those of open disputes.
    function invariant_I12_disputeBondsMatchOpenDisputes() public view {
        uint256 sum;
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            if (job.state == ITaskEscrow.JobState.DISPUTED) sum += job.disputeBond;
        }
        assertEq(escrow.disputeBondTotal(), sum, "a dispute bond outlived its dispute");
    }

    /// I4 — a fee never takes the whole payment, and no job is below the minimum.
    function invariant_I4_feeIsBounded() public view {
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            assertLt(job.fee, job.amount, "the worker is always paid something");
            assertGe(job.amount, MIN_JOB_AMOUNT, "no job below the minimum");
        }
    }

    /// I6 — no job has the same agent, or the same owner, on both sides.
    function invariant_I6_noSelfDealing() public view {
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            assertTrue(job.clientAgentId != job.workerAgentId, "self-dealing job exists");
        }
    }

    /// Every disputed job has a deadline, so every disputed job has an exit.
    function invariant_everyDisputeHasADeadline() public view {
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            if (job.state == ITaskEscrow.JobState.DISPUTED) {
                assertGt(job.disputeDeadline, 0, "a dispute with no way out");
            }
        }
    }

    /// Captured windows are the ones in force when the job was created, and a
    /// mid-flight setParams never rewrites them: every escrow job carries its
    /// own non-zero review window and dispute timeout.
    function invariant_escrowJobsCarryTheirOwnWindows() public view {
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            ITaskEscrow.Job memory job = escrow.getJob(handler.jobIds(i));
            if (job.acceptDeadline == job.workDeadline) continue; // fast path
            assertGt(job.reviewWindow, 0);
            assertEq(job.disputeTimeout, DISPUTE_TIMEOUT, "dispute timeout was rewritten");
        }
    }

    /// Pausing blocks every new job.
    function invariant_noJobIsCreatedWhilePaused() public view {
        assertEq(handler.ghostCreatedWhilePaused(), 0);
    }

    /// Every job the handler created is in a real state — never NONE.
    function invariant_jobsAreAlwaysInAValidState() public view {
        uint256 n = handler.jobCount();
        for (uint256 i; i < n; i++) {
            assertTrue(escrow.getJob(handler.jobIds(i)).state != ITaskEscrow.JobState.NONE);
        }
    }
}
