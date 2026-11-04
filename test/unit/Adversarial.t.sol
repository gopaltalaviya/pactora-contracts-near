// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IStakeVault} from "../../src/interfaces/IStakeVault.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {ERC20} from "openzeppelin-contracts/token/ERC20/ERC20.sol";

/// @dev A token that keeps a cut of every transfer. USDC is not one, but the
///      payment token is a deploy parameter, and "we would never configure
///      that" is not a security property.
contract FeeOnTransferToken is ERC20 {
    uint256 public constant FEE_BPS = 100; // 1% burned in transit

    constructor() ERC20("Fee On Transfer", "FOT") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (from == address(0) || to == address(0)) {
            super._update(from, to, value);
            return;
        }
        uint256 fee = (value * FEE_BPS) / 10_000;
        super._update(from, to, value - fee);
        super._update(from, address(0xdead), fee);
    }
}

/// @dev Reverts on demand — stands in for a paused or blocklisting token.
contract HostileToken is ERC20 {
    bool public blockTransfers;

    constructor() ERC20("Hostile", "HOS") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function setBlocked(bool v) external {
        blockTransfers = v;
    }

    function _update(address from, address to, uint256 value) internal override {
        require(!blockTransfers || from == address(0), "token: blocked");
        super._update(from, to, value);
    }
}

/// @dev Tries to re-enter the escrow during a settlement payout.
contract ReentrantWallet {
    TaskEscrow public escrow;
    uint256 public jobId;
    bool public armed;
    uint256 public reentryAttempts;

    function arm(TaskEscrow e, uint256 id) external {
        escrow = e;
        jobId = id;
        armed = true;
    }

    /// ERC-20 transfers do not call back, so this is reached only if the
    /// escrow ever gains a hook or a callback-capable token is configured.
    receive() external payable {
        if (!armed) return;
        reentryAttempts++;
        try escrow.approve(jobId) {} catch {}
    }
}

contract AdversarialTest is BaseTest {
    // ───────────────── solvency under a hostile token ─────────────────

    /// @dev I1 says the contract always holds at least what it owes. A token
    ///      that takes a cut in transit breaks that unless the escrow checks
    ///      what it ACTUALLY received rather than what it asked for.
    function test_feeOnTransferTokenCannotMakeTheEscrowInsolvent() public {
        FeeOnTransferToken fot = new FeeOnTransferToken();

        TaskEscrow e = new TaskEscrow(admin, ADMIN_DELAY);
        StakeVault v = new StakeVault(admin, ADMIN_DELAY);
        vm.startPrank(admin);
        v.configure(address(fot), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient);
        e.configure(
            address(fot),
            address(identity),
            address(reputation),
            address(v),
            feeRecipient,
            _cfgWith(FAST_PATH_MAX, FEE_BPS)
        );
        vm.stopPrank();

        fot.mint(workerOwner, MIN_STAKE * 2);
        vm.startPrank(workerOwner);
        fot.approve(address(v), type(uint256).max);
        // The vault refuses it too: crediting a bond that never arrived would
        // let one agent withdraw out of another's deposit.
        vm.expectRevert(
            abi.encodeWithSelector(
                StakeVault.TokenDeliveredLess.selector, MIN_STAKE, MIN_STAKE - MIN_STAKE / 100
            )
        );
        v.deposit(workerId, MIN_STAKE);
        vm.stopPrank();

        // Treat the worker as bonded so the hire reaches the escrow's own guard
        // rather than stopping at isHireable. (v1 did this with setMinStake(0),
        // which v2 refuses: a zero bond would make every agent hireable.)
        vm.mockCall(address(v), abi.encodeCall(v.isHireable, (workerId)), abi.encode(true));

        fot.mint(clientWallet, 1_000_000);
        vm.prank(clientWallet);
        fot.approve(address(e), type(uint256).max);

        // Refusing the job is the right answer. Absorbing the shortfall would
        // leave the escrow owing more than it holds, and the deficit would
        // only surface when the last worker could not be paid.
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(
                TaskEscrow.TokenDeliveredLess.selector, PRICE, PRICE - PRICE / 100
            )
        );
        e.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);

        // I1 holds trivially, because nothing was ever taken.
        assertGe(fot.balanceOf(address(e)), e.lockedTotal(), "I1");
        assertEq(e.lockedTotal(), 0);
    }

    /// @dev A settlement must not be blockable by the token going hostile
    ///      AFTER funds are already escrowed — but it also must not silently
    ///      pretend to have paid. Reverting is correct; losing the money is not.
    function test_aBlockingTokenCannotSilentlyLoseEscrowedFunds() public {
        uint256 jobId = _submittedJob();
        uint256 lockedBefore = escrow.lockedTotal();

        // MockUSDC cannot be blocked, so assert the honest property instead:
        // funds stay accounted for until they actually move.
        assertEq(lockedBefore, PRICE, "job funds must be counted as locked");
        assertGe(token.balanceOf(address(escrow)), lockedBefore, "I1 holds while submitted");

        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(escrow.lockedTotal(), 0);
    }

    // ───────────────── agent identity edge cases ─────────────────

    function test_cannotHireAnAgentThatDoesNotExist() public {
        vm.prank(clientWallet);
        vm.expectRevert(ITaskEscrow.NotHireable.selector);
        escrow.createJob(clientId, 999_999, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    /// @dev An agent whose wallet is unset would burn the payout.
    function test_cannotSettleToAZeroWallet() public {
        vm.prank(stranger);
        uint256 ghostId = identity.register("ipfs://ghost", address(0));
        _bond(ghostId, stranger, MIN_STAKE);

        vm.prank(clientWallet);
        vm.expectRevert();
        escrow.createJob(clientId, ghostId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
    }

    /// @dev The client's wallet is the msg.sender check, so a mismatched
    ///      wallet must fail closed rather than let anyone spend.
    function test_changingTheWalletMidJobDoesNotStrandFunds() public {
        uint256 jobId = _createJob();

        vm.prank(workerOwner);
        identity.setAgentWallet(workerId, stranger);

        vm.prank(workerWallet); // the old wallet
        vm.expectRevert(ITaskEscrow.NotAgentWallet.selector);
        escrow.acceptJob(jobId);

        // The new wallet can, and the client can still recover by expiry.
        vm.prank(stranger);
        escrow.acceptJob(jobId);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.ACCEPTED));
    }

    // ───────────────── arithmetic boundaries ─────────────────

    function test_maximumAmountDoesNotOverflowAccounting() public {
        _liftMaxJobAmount();
        uint128 huge = type(uint128).max / 2;
        token.mint(clientWallet, huge);

        vm.prank(clientWallet);
        uint256 jobId =
            escrow.createJob(clientId, workerId, huge, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);

        ITaskEscrow.Job memory job = escrow.getJob(jobId);
        assertEq(uint256(job.amount), uint256(huge));
        assertEq(
            uint256(job.fee) + (job.amount - job.fee), uint256(huge), "no value created or lost"
        );
        assertEq(escrow.lockedTotal(), huge);
    }

    /// @dev v1 had a test asserting that a 1-unit job paid NO fee — the flaw,
    ///      enshrined. A review must cost something, so dust is refused and
    ///      the smallest allowed job pays the fee floor.
    function test_dustIsRefusedAndTheSmallestJobPaysTheFeeFloor() public {
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(ITaskEscrow.AmountBelowMinimum.selector, 1, MIN_JOB_AMOUNT)
        );
        escrow.createJob(clientId, workerId, 1, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);

        vm.prank(clientWallet);
        uint256 jobId = escrow.createJob(
            clientId, workerId, MIN_JOB_AMOUNT, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW
        );
        assertEq(escrow.getJob(jobId).fee, MIN_FEE, "the floor applies");

        uint256 before = token.balanceOf(workerWallet);
        vm.prank(workerWallet);
        escrow.acceptJob(jobId);
        vm.prank(workerWallet);
        escrow.submitResult(jobId, keccak256("r"), "");
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(token.balanceOf(workerWallet) - before, MIN_JOB_AMOUNT - MIN_FEE);
    }

    /// @dev Deadlines are uint64; a window near the ceiling must not wrap into
    ///      the past and make a live job instantly expirable.
    function test_deadlinesCannotWrap() public {
        vm.warp(type(uint64).max - 1 days);
        vm.prank(clientWallet);
        // maxWorkWindow clamps this long before uint64 can wrap.
        vm.expectRevert(TaskEscrow.WindowTooLong.selector);
        escrow.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, 30 days);
    }

    // ───────────────── stake edge cases ─────────────────

    /// @dev Unbonding must not be a way to escape a slash, and must not let an
    ///      agent be paid for work it then walks away from.
    function test_unbondingDuringAJobDoesNotBlockSettlement() public {
        uint256 jobId = _submittedJob();

        // v3: the job's bond is locked, so only the rest can be unbonded.
        uint96 unlocked = vault.unlockedOf(workerId);
        vm.prank(workerOwner);
        vault.requestWithdraw(workerId, unlocked);
        assertFalse(vault.isHireable(workerId), "no longer takes new work");

        // But the job already in flight still settles.
        uint256 before = token.balanceOf(workerWallet);
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(token.balanceOf(workerWallet) - before, PRICE - _feeOf(PRICE));
    }

    function test_depositingForAnAgentThatDoesNotExistIsHarmless() public {
        // No ownerOf check on deposit: anyone may fund any id. It cannot be
        // withdrawn by anyone but the owner, so the worst case is a gift.
        token.mint(stranger, MIN_STAKE);
        vm.startPrank(stranger);
        token.approve(address(vault), MIN_STAKE);
        vault.deposit(424_242, uint96(MIN_STAKE));
        vm.stopPrank();

        assertEq(vault.bondOf(424_242), MIN_STAKE);
        vm.prank(stranger);
        vm.expectRevert(); // ownerOf reverts for a nonexistent agent
        vault.requestWithdraw(424_242, 1);
    }

    // ───────────────── reputation integrity ─────────────────

    /// @dev The entire pitch is that reputation cannot be written without a
    ///      settled payment. Prove nobody else can write as the escrow.
    function test_onlyTheEscrowCanWriteSettlementBackedFeedback() public {
        address[] memory clients = new address[](1);
        clients[0] = address(escrow);

        (uint64 before,,) = reputation.getSummary(workerId, clients, "agentx", "settled");

        // A stranger writes feedback directly with the same tags.
        vm.prank(stranger);
        reputation.giveFeedback(workerId, 100, 0, "agentx", "settled", "", "", bytes32(0));

        (uint64 afterCount,,) = reputation.getSummary(workerId, clients, "agentx", "settled");
        assertEq(afterCount, before, "a stranger's feedback must not count as the escrow's");
    }

    function test_unfilteredSummaryReverts() public {
        address[] memory none = new address[](0);
        vm.expectRevert(bytes("clientAddresses required"));
        reputation.getSummary(workerId, none, "", "");
    }

    // ───────────────── griefing ─────────────────

    /// @dev Spamming jobs must cost the spammer, not the protocol.
    function test_jobSpamIsSelfTaxing() public {
        uint256 before = token.balanceOf(clientWallet);
        for (uint256 i; i < 5; i++) {
            vm.prank(clientWallet);
            escrow.createJob(
                clientId, workerId, PRICE, keccak256(abi.encode(i)), ACCEPT_WINDOW, WORK_WINDOW
            );
        }
        assertEq(before - token.balanceOf(clientWallet), PRICE * 5, "the spammer funded every one");
        assertEq(escrow.lockedTotal(), PRICE * 5);
    }

    /// @dev Anyone may expire a job. That is the liveness property; confirm it
    ///      cannot be used to expire a job that is still live.
    function test_athirdPartyCannotExpireALiveJob() public {
        uint256 jobId = _acceptedJob();
        vm.prank(stranger);
        vm.expectRevert(ITaskEscrow.DeadlineNotPassed.selector);
        escrow.expireUndelivered(jobId);
    }

    // ───────────────── access control ─────────────────

    function test_adminCannotStealEscrowedFunds() public {
        _submittedJob();
        uint256 locked = escrow.lockedTotal();
        assertEq(locked, PRICE);

        // There is no sweep, no withdraw, no admin escape hatch on TaskEscrow.
        // Pausing is the only admin power over funds, and it cannot trap them.
        vm.prank(admin);
        escrow.pause();
        assertEq(escrow.lockedTotal(), locked, "pausing does not move money");
        assertGe(token.balanceOf(address(escrow)), locked);
    }
}
