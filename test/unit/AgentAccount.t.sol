// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccount} from "../../src/AgentAccount.sol";
import {AgentAccountCore} from "../../src/AgentAccountCore.sol";
import {AgentAccountFactory} from "../../src/AgentAccountFactory.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {IERC20} from "openzeppelin-contracts/token/ERC20/IERC20.sol";

/// @dev An owner that is a contract, approving exactly one digest.
contract ContractOwner {
    bytes32 public approved;

    function approve(bytes32 h) external {
        approved = h;
    }

    function isValidSignature(bytes32 h, bytes calldata) external view returns (bytes4) {
        return h == approved ? bytes4(0x1626ba7e) : bytes4(0xffffffff);
    }
}

/// @dev A call target that is also a granted session key and tries to
///      re-enter `execute` while its own call is still running.
contract Reenterer {
    function poke(address account, bytes calldata inner) external {
        AgentAccountCore(payable(account)).execute(address(this), inner);
    }

    function noop() external {}
}

/// @dev The threat model these tests encode: the AGENTX signer is compromised
///      and the attacker holds a live session key. The caps must still hold.
contract AgentAccountTest is BaseTest {
    AgentAccountFactory internal factory;
    AgentAccount internal account;

    address internal agentOwner = makeAddr("agentOwner");
    address internal sessionKey = makeAddr("sessionKey");
    address internal attacker = makeAddr("attacker");

    uint128 internal constant PER_TASK_CAP = 100_000; // 0.10 USDC
    uint128 internal constant DAILY_CAP = 250_000; // 0.25 USDC

    uint256 internal accountAgentId;

    function setUp() public virtual override {
        super.setUp();

        // These tests probe the ACCOUNT's caps, so the escrow's own fast-path
        // ceiling must be raised out of the way — otherwise directPay rejects
        // first and the account limits are never reached.
        vm.prank(admin);
        escrow.setParams(_cfgWith(1_000_000, FEE_BPS), feeRecipient);

        factory = new AgentAccountFactory(address(token));
        account = _makeAccount();

        // The account is this agent's registered wallet.
        vm.prank(agentOwner);
        accountAgentId = identity.register("ipfs://acct", address(account));
        _bond(accountAgentId, agentOwner, MIN_STAKE);

        token.mint(address(account), 10_000_000);

        vm.startPrank(agentOwner);
        account.setAllowedCall(address(escrow), TaskEscrow.directPay.selector, true);
        account.setAllowedCall(address(escrow), TaskEscrow.createJob.selector, true);
        account.setAllowance(address(escrow), type(uint256).max);
        account.grantSessionKey(sessionKey, uint64(block.timestamp + 1 days), DAILY_CAP);
        vm.stopPrank();
    }

    /// @dev The wallet under test. AgentDelegateTest overrides this, so every
    ///      test here also runs against an EIP-7702 delegated EOA.
    function _makeAccount() internal virtual returns (AgentAccount a) {
        address predicted = factory.predictAddress(agentOwner, bytes32("agent-1"));
        vm.prank(agentOwner); // only the owner may create their account (M6)
        a = AgentAccount(
            payable(factory.createAccount(
                    agentOwner,
                    bytes32("agent-1"),
                    AgentAccountCore.Policy({
                        perTaskCap: PER_TASK_CAP, dailyCap: DAILY_CAP, allowlistOnly: true
                    })
                ))
        );
        assertEq(address(a), predicted, "address must be predictable before deployment");
    }

    // ─────────────────── access control ───────────────────

    /// @dev Nothing tested this. `onlyOwner` showed as an uncovered branch in
    ///      the coverage report, which means no test had ever called an
    ///      owner-only function as a stranger — so removing the modifier from
    ///      any of these six would not have failed a single test.
    ///
    ///      This is the contract that bounds a compromised agent. Its access
    ///      control is the reason the bound holds.
    function test_onlyOwner_strangerCannotSetPolicy() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: type(uint128).max, dailyCap: type(uint128).max, allowlistOnly: false
            })
        );
    }

    function test_onlyOwner_strangerCannotGrantThemselvesASessionKey() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.grantSessionKey(attacker, uint64(block.timestamp) + 1 hours, type(uint128).max);
    }

    function test_onlyOwner_strangerCannotAllowlistACall() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setAllowedCall(attacker, bytes4(0xdeadbeef), true);
    }

    function test_onlyOwner_strangerCannotGrantAnAllowance() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setAllowance(attacker, type(uint256).max);
    }

    /// @dev The one that would drain it outright.
    function test_onlyOwner_strangerCannotSweepTheFunds() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweep(attacker, 1);
    }

    /// @dev A live session key may SPEND within its budget, but it is not the
    ///      owner: it must not be able to raise its own limits.
    function test_onlyOwner_aSessionKeyCannotRaiseItsOwnCaps() public {
        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: type(uint128).max, dailyCap: type(uint128).max, allowlistOnly: false
            })
        );
    }

    function test_onlyOwner_aSessionKeyCannotSweep() public {
        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweep(sessionKey, 1);
    }

    // ─────────────────── session key lifetime ───────────────────

    /// @dev A session key exists so that compromise of an online signer is
    ///      bounded IN TIME. One granted forever is a permanent key with extra
    ///      steps, and a bound that relies on the caller passing the right
    ///      number is not a control. T7 claims this; the contract enforces it.
    function test_grantSessionKey_rejectsLifetimeBeyondTheCeiling() public {
        uint64 tooLong = uint64(block.timestamp + 1 days + 1);
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.SessionKeyTtlInvalid.selector,
                tooLong,
                uint64(block.timestamp) + 1 days
            )
        );
        account.grantSessionKey(attacker, tooLong, 1);
    }

    function test_grantSessionKey_rejectsAnEternalKey() public {
        vm.prank(agentOwner);
        vm.expectRevert();
        account.grantSessionKey(attacker, type(uint64).max, 1);
    }

    /// @dev Exactly at the ceiling is allowed: an off-by-one here would make
    ///      the documented 24h grant itself impossible.
    function test_grantSessionKey_allowsExactlyTheCeiling() public {
        vm.prank(agentOwner);
        account.grantSessionKey(attacker, uint64(block.timestamp) + 1 days, 1);
        (uint64 expiry,,) = account.sessionKeys(attacker);
        assertEq(expiry, uint64(block.timestamp) + 1 days, "a 24h grant must be allowed");
    }

    /// @dev A grant that expired before it was made would emit a
    ///      SessionKeyGranted event for a key that never worked, which reads
    ///      in the log exactly like a working key.
    function test_grantSessionKey_rejectsAnExpiryInThePast() public {
        vm.warp(block.timestamp + 10 days);
        vm.prank(agentOwner);
        vm.expectRevert();
        account.grantSessionKey(attacker, uint64(block.timestamp - 1), 1);
    }

    function test_grantSessionKey_rejectsTheCurrentTimestamp() public {
        vm.prank(agentOwner);
        vm.expectRevert();
        account.grantSessionKey(attacker, uint64(block.timestamp), 1);
    }

    /// @dev Rotation must keep working: the ceiling constrains each grant, not
    ///      the total life of a key that is re-granted daily.
    function test_grantSessionKey_canBeRenewedAfterTheCeilingPasses() public {
        vm.warp(block.timestamp + 2 days);
        vm.prank(agentOwner);
        account.grantSessionKey(sessionKey, uint64(block.timestamp) + 1 days, DAILY_CAP);

        vm.prank(sessionKey);
        account.execute(address(escrow), _payCall(10_000));
    }

    function _payCall(uint128 amount) internal view returns (bytes memory) {
        return
            abi.encodeCall(
                TaskEscrow.directPay, (accountAgentId, workerId, amount, keccak256("spec"))
            );
    }

    // ───────────────────── it actually works ─────────────────────

    function test_sessionKeyCanSpendWithinPolicy() public {
        uint256 before = token.balanceOf(workerWallet);

        vm.prank(sessionKey);
        account.execute(address(escrow), _payCall(MICRO));

        assertEq(token.balanceOf(workerWallet) - before, MICRO - _feeOf(MICRO), "worker paid");
        assertEq(account.spentToday(), MICRO, "spend measured from the real balance delta");
    }

    function test_ownerCanSpendDirectly() public {
        vm.prank(agentOwner);
        account.execute(address(escrow), _payCall(MICRO));
        assertEq(account.spentToday(), MICRO);
    }

    // ───────────────────── the caps hold ─────────────────────

    function test_perTaskCapBlocksASingleLargePayment() public {
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.PerTaskCapExceeded.selector, PER_TASK_CAP + 1, PER_TASK_CAP
            )
        );
        account.execute(address(escrow), _payCall(PER_TASK_CAP + 1));
    }

    /// @dev The runaway-loop case: many individually-legal payments.
    function test_dailyCapStopsARunawayLoop() public {
        uint128 each = 100_000; // == perTaskCap, so each call alone is legal
        for (uint256 i; i < 2; i++) {
            vm.prank(sessionKey);
            account.execute(address(escrow), _payCall(each));
        }
        assertEq(account.spentToday(), 200_000);
        assertEq(account.dailyRemaining(), 50_000);

        // The third would exceed the daily cap and must revert outright.
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.DailyCapExceeded.selector, each, uint128(50_000)
            )
        );
        account.execute(address(escrow), _payCall(each));

        assertEq(account.spentToday(), 200_000, "a rejected call must leave no trace");
    }

    /// @dev v3: the window rolls, and a spend counts for up to 25 hours (the
    ///      cap errs safe by up to one hour; V3RollingCap covers the edges).
    function test_dailyCapRollsAfterADay() public {
        vm.prank(sessionKey);
        account.execute(address(escrow), _payCall(100_000));
        assertEq(account.dailyRemaining(), 150_000);

        vm.warp(block.timestamp + 25 hours);
        vm.prank(agentOwner);
        account.grantSessionKey(sessionKey, uint64(block.timestamp + 1 days), DAILY_CAP);
        assertEq(account.dailyRemaining(), DAILY_CAP, "window rolled");

        vm.prank(sessionKey);
        account.execute(address(escrow), _payCall(100_000));
        assertEq(account.spentToday(), 100_000, "counter reset, not accumulated");
    }

    // ───────────────────── compromise containment ─────────────────────

    function test_strangerWithNoSessionKeyCannotSpend() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.execute(address(escrow), _payCall(MICRO));
    }

    function test_expiredSessionKeyCannotSpend() public {
        vm.warp(block.timestamp + 2 days);
        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.SessionKeyExpired.selector);
        account.execute(address(escrow), _payCall(MICRO));
    }

    function test_nonAllowlistedTargetRejected() public {
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(AgentAccountCore.TargetNotAllowed.selector, address(token))
        );
        account.execute(address(token), abi.encodeCall(IERC20.transfer, (attacker, 1)));
    }

    function test_nonAllowlistedSelectorRejected() public {
        // The token is an allowed target — for reading its balance only.
        vm.prank(agentOwner);
        account.setAllowedCall(address(token), IERC20.balanceOf.selector, true);

        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.SelectorNotAllowed.selector, IERC20.transfer.selector
            )
        );
        account.execute(address(token), abi.encodeCall(IERC20.transfer, (attacker, 1)));
    }

    /// @dev A standing allowance would let a spender move funds outside every
    ///      cap, so it must be unreachable from the session-key path.
    function test_approveCanNeverBeGrantedThroughExecute() public {
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.ForbiddenSelector.selector, IERC20.approve.selector
            )
        );
        account.setAllowedCall(address(token), IERC20.approve.selector, true);
    }

    /// @dev Every selector that grants a standing allowance, not only `approve`:
    ///      real USDC has `increaseAllowance`, and v1 would have allowed it.
    function test_noAllowanceGrantingSelectorCanBeAllowed() public {
        bytes4[3] memory forbidden = [bytes4(0x39509351), bytes4(0xa22cb465), bytes4(0xd505accf)];
        for (uint256 i; i < forbidden.length; i++) {
            vm.prank(agentOwner);
            vm.expectRevert(
                abi.encodeWithSelector(AgentAccountCore.ForbiddenSelector.selector, forbidden[i])
            );
            account.setAllowedCall(address(token), forbidden[i], true);
        }
    }

    /// @dev The account's own owner-only functions must not become reachable
    ///      by a session key through `execute`.
    /// @dev The allowlist alone would let this through: allowlistOnly off and
    ///      `sweep` allowed on every target by the wildcard. Only the explicit
    ///      self-target check stops a session key reaching the owner's controls
    ///      — and for an EIP-7702 delegate, whose owner IS address(this), a
    ///      self-call would pass `onlyOwner` and sweep everything.
    function test_theWildcardCannotReachTheAccountsOwnControls() public {
        vm.startPrank(agentOwner);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: PER_TASK_CAP, dailyCap: DAILY_CAP, allowlistOnly: false
            })
        );
        account.setAllowedCall(address(0), AgentAccountCore.sweep.selector, true);
        vm.stopPrank();

        uint256 bal = token.balanceOf(address(account));
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(AgentAccountCore.TargetNotAllowed.selector, address(account))
        );
        account.execute(address(account), abi.encodeCall(AgentAccountCore.sweep, (attacker, bal)));
        assertEq(token.balanceOf(attacker), 0);
    }

    function test_theAllowlistIsReadable() public {
        assertTrue(account.allowedCall(address(escrow), TaskEscrow.directPay.selector));
        assertFalse(account.allowedCall(address(escrow), TaskEscrow.approve.selector));
        assertEq(account.allowedCallCount(address(escrow)), 2);
        vm.prank(agentOwner);
        account.setAllowedCall(address(escrow), TaskEscrow.directPay.selector, false);
        assertFalse(account.allowedCall(address(escrow), TaskEscrow.directPay.selector));
        assertEq(account.allowedCallCount(address(escrow)), 1);
    }

    function test_executeCannotBeReentered() public {
        Reenterer r = new Reenterer();
        vm.startPrank(agentOwner);
        account.setAllowedCall(address(r), Reenterer.poke.selector, true);
        account.setAllowedCall(address(r), Reenterer.noop.selector, true);
        account.grantSessionKey(address(r), uint64(block.timestamp + 1 hours), DAILY_CAP);
        vm.stopPrank();

        bytes memory inner = abi.encodeCall(Reenterer.noop, ());
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.CallFailed.selector,
                abi.encodeWithSelector(AgentAccountCore.ReentrancyGuardReentrantCall.selector)
            )
        );
        account.execute(address(r), abi.encodeCall(Reenterer.poke, (address(account), inner)));
    }

    function test_theAccountCannotBeATargetOfItsOwnExecute() public {
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(AgentAccountCore.TargetNotAllowed.selector, address(account))
        );
        account.setAllowedCall(address(account), AgentAccountCore.sweep.selector, true);

        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(AgentAccountCore.TargetNotAllowed.selector, address(account))
        );
        account.execute(address(account), abi.encodeCall(AgentAccountCore.sweep, (attacker, 1)));
    }

    // ─────────────────── v2: owner controls ───────────────────

    function test_sweepNativeRecoversTheNativeCoin() public {
        vm.deal(address(account), 1 ether);
        address payable to = payable(makeAddr("rescue"));
        vm.prank(agentOwner);
        account.sweepNative(to, 1 ether);
        assertEq(to.balance, 1 ether);

        vm.deal(address(account), 1 ether);
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweepNative(payable(attacker), 1 ether);
    }

    function test_ownershipTransferTakesTwoSteps() public virtual {
        address next = makeAddr("nextOwner");
        vm.prank(agentOwner);
        account.transferOwnership(next);
        assertEq(account.owner(), agentOwner, "nothing changes until accepted");

        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.acceptOwnership();

        vm.prank(next);
        account.acceptOwnership();
        assertEq(account.owner(), next);
        assertEq(account.pendingOwner(), address(0));
    }

    function test_disallowingACallTakesEffect() public {
        vm.prank(agentOwner);
        account.setAllowedCall(address(escrow), TaskEscrow.directPay.selector, false);

        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.SelectorNotAllowed.selector, TaskEscrow.directPay.selector
            )
        );
        account.execute(address(escrow), _payCall(MICRO));
    }

    function test_sessionKeyBudgetIsIndependentOfTheDailyCap() public {
        vm.prank(agentOwner);
        account.grantSessionKey(sessionKey, uint64(block.timestamp + 1 days), 30_000);

        vm.prank(sessionKey);
        account.execute(address(escrow), _payCall(MICRO)); // 20_000, fits

        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.SessionBudgetExceeded.selector);
        account.execute(address(escrow), _payCall(MICRO)); // would total 40_000
    }

    // ───────────────────── owner keeps control ─────────────────────

    function test_ownerCanRevokeAndStopEverythingImmediately() public {
        vm.prank(agentOwner);
        account.revokeSessionKey(sessionKey);

        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.execute(address(escrow), _payCall(MICRO));
    }

    /// @dev A signer that detects its own compromise should be able to disarm
    ///      without waiting for a human.
    function test_sessionKeyCanRevokeItself() public {
        vm.prank(sessionKey);
        account.revokeSessionKey(sessionKey);

        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.execute(address(escrow), _payCall(MICRO));
    }

    function test_ownerCanSweep() public virtual {
        uint256 bal = token.balanceOf(address(account));
        vm.prank(agentOwner);
        account.sweep(agentOwner, bal);
        assertEq(token.balanceOf(agentOwner), bal);
        assertEq(token.balanceOf(address(account)), 0);
    }

    function test_onlyOwnerMaySweepOrSetPolicy() public {
        vm.startPrank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweep(attacker, 1);

        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: type(uint128).max, dailyCap: type(uint128).max, allowlistOnly: false
            })
        );

        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.grantSessionKey(attacker, uint64(block.timestamp + 1 days), type(uint128).max);
        vm.stopPrank();
    }

    function test_implementationCannotBeInitialized() public virtual {
        AgentAccount impl = AgentAccount(payable(factory.implementation()));
        vm.expectRevert();
        impl.initialize(
            attacker,
            address(token),
            AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: false})
        );
    }

    function test_failedInnerCallBubblesUp() public {
        // Self-dealing: the escrow will reject it, and execute must not swallow it.
        vm.prank(sessionKey);
        vm.expectRevert();
        account.execute(
            address(escrow),
            abi.encodeCall(
                TaskEscrow.directPay, (accountAgentId, accountAgentId, MICRO, keccak256("s"))
            )
        );
    }

    function testFuzz_spendNeverExceedsTheDailyCap(uint128[5] calldata amounts) public {
        uint128 total;
        for (uint256 i; i < amounts.length; i++) {
            uint128 amount = uint128(bound(amounts[i], 1, PER_TASK_CAP));
            vm.prank(sessionKey);
            try account.execute(address(escrow), _payCall(amount)) {
                total += amount;
            } catch {}
        }
        assertEq(account.spentToday(), total);
        assertLe(account.spentToday(), DAILY_CAP, "the cap is absolute");
    }

    function test_ownerCanTightenPolicyOnALiveAccount() public {
        vm.prank(agentOwner);
        account.setPolicy(
            AgentAccountCore.Policy({perTaskCap: 1_000, dailyCap: 2_000, allowlistOnly: true})
        );

        (uint128 perTask, uint128 daily,) = account.policy();
        assertEq(perTask, 1_000);
        assertEq(daily, 2_000);

        // The new, tighter cap applies immediately to the existing session key.
        vm.prank(sessionKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.PerTaskCapExceeded.selector, MICRO, uint128(1_000)
            )
        );
        account.execute(address(escrow), _payCall(MICRO));
    }

    function test_aThirdPartyCannotRevokeSomeoneElsesSessionKey() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.revokeSessionKey(sessionKey);
    }

    // ───────────────────── rolling daily cap (v3) ─────────────────────
    //
    // v3 (owner decision 2026-10-25): the daily cap is ROLLING. v2's fixed
    // window let a full cap be spent just before it reset and another just
    // after — 2x in two minutes. Now no 24-hour period ever sees more than
    // `dailyCap`. Spend is counted in hourly buckets, so a spend counts for
    // between 24 and 25 hours: the cap errs safe by up to one hour.

    function _spend(uint128 amount) internal {
        vm.prank(agentOwner);
        account.execute(address(escrow), _payCall(amount));
    }

    /// @dev The v2 exploit, exactly.
    function test_theCapCannotBeDoubledAcrossTheOldWindowBoundary() public {
        vm.warp(block.timestamp + 1 days - 60);
        _spend(100_000);
        _spend(100_000);
        _spend(50_000); // the whole cap, a minute before v2's reset
        vm.warp(block.timestamp + 120); // a minute after it
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.DailyCapExceeded.selector, uint128(10_000), uint128(0)
            )
        );
        account.execute(address(escrow), _payCall(10_000));
    }

    function test_aSpendStillCountsAfterExactly24Hours() public {
        _spend(100_000);
        _spend(100_000);
        _spend(50_000);
        vm.warp(block.timestamp + 24 hours);
        assertEq(account.dailyRemaining(), 0, "errs safe: still inside its 25th hour");
        vm.warp(block.timestamp + 1 hours);
        assertEq(account.dailyRemaining(), DAILY_CAP, "gone after 25 hours at most");
    }

    function test_capacityReturnsSpendBySpend() public {
        uint256 t0 = block.timestamp - (block.timestamp % 1 hours); // start of an hour
        vm.warp(t0);
        _spend(100_000);
        vm.warp(t0 + 5 hours);
        _spend(100_000);
        assertEq(account.dailyRemaining(), 50_000);
        assertEq(account.nextReleaseAt(), t0 + 25 hours, "the first spend leaves first");

        vm.warp(t0 + 25 hours);
        assertEq(account.dailyRemaining(), 150_000, "first spend released");
        assertEq(account.nextReleaseAt(), t0 + 30 hours);
        vm.warp(t0 + 30 hours);
        assertEq(account.dailyRemaining(), DAILY_CAP, "both released");
        assertEq(account.nextReleaseAt(), 0, "nothing left to release");
    }

    /// @dev The roll must hand back an expired spend's capacity to the NEXT
    ///      spend, not only to the views (coverage showed nothing exercised it).
    function test_aSpendAfterTheOldestExpiresUsesItsCapacity() public {
        uint256 t0 = block.timestamp - (block.timestamp % 1 hours);
        vm.warp(t0);
        _spend(100_000);
        vm.warp(t0 + 5 hours);
        _spend(100_000);
        vm.warp(t0 + 25 hours); // the first spend has left; the second has not
        _spend(100_000);
        _spend(50_000);
        assertEq(account.spentToday(), 250_000);
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.DailyCapExceeded.selector, uint128(10_000), uint128(0)
            )
        );
        account.execute(address(escrow), _payCall(10_000));
    }

    function test_aLongIdleResetsTheWindow() public {
        _spend(100_000);
        vm.warp(block.timestamp + 100 hours);
        _spend(30_000);
        assertEq(account.spentToday(), 30_000, "only the recent spend counts");
        assertEq(account.dailyRemaining(), DAILY_CAP - 30_000);
    }

    function test_lowerCapAppliesToSpendAlreadyCounted() public {
        _spend(100_000);
        vm.prank(agentOwner);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: PER_TASK_CAP, dailyCap: 50_000, allowlistOnly: true
            })
        );
        assertEq(account.dailyRemaining(), 0);
    }

    // ───────────────────── ERC-1271: the owner signs for the account (v3) ─────────────────────
    //
    // The canonical ERC-8004 registry sets an agent's payout wallet only with a
    // signature from that wallet — ECDSA, or ERC-1271 for a contract. Without
    // this an AgentAccount could not be an agent's wallet on the real registry.
    // Only the OWNER signs (a session key never may: a stolen hot key would sign
    // approvals for anything that accepts ERC-1271), and the account binds the
    // hash to itself and the chain, so one owner's signature for account A is
    // not also valid for account B.

    bytes4 internal constant MAGIC = 0x1626ba7e;

    function _wrapped(AgentAccount a, bytes32 hash) internal view returns (bytes32) {
        return a.accountMessageHash(hash);
    }

    function _ownedAccount(uint256 ownerKey, bytes32 salt) internal returns (AgentAccount a) {
        address o = vm.addr(ownerKey);
        vm.prank(o);
        a = AgentAccount(
            payable(factory.createAccount(
                    o,
                    salt,
                    AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true})
                ))
        );
    }

    function test_theOwnersSignatureIsValidForTheAccount() public virtual {
        (, uint256 k) = makeAddrAndKey("signer-owner");
        AgentAccount a = _ownedAccount(k, "s1");
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(k, _wrapped(a, h));
        assertEq(a.isValidSignature(h, abi.encodePacked(r, s_, v)), MAGIC);
    }

    function test_aSignatureOverTheBareHashIsRefused() public virtual {
        (, uint256 k) = makeAddrAndKey("signer-owner");
        AgentAccount a = _ownedAccount(k, "s1");
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(k, h);
        assertTrue(a.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC);
    }

    function test_oneAccountsSignatureIsNotValidForAnother() public virtual {
        (, uint256 k) = makeAddrAndKey("signer-owner");
        AgentAccount a = _ownedAccount(k, "s1");
        AgentAccount b = _ownedAccount(k, "s2"); // same owner
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(k, _wrapped(a, h));
        assertTrue(b.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC);
    }

    function test_aSessionKeysSignatureIsRefused() public {
        (address key, uint256 k) = makeAddrAndKey("hot-key");
        vm.prank(agentOwner);
        account.grantSessionKey(key, uint64(block.timestamp + 1 hours), 1);
        bytes32 h = keccak256("anything");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(k, _wrapped(account, h));
        assertTrue(account.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC);
    }

    function test_aMalformedSignatureIsRefusedNotReverted() public view {
        assertTrue(account.isValidSignature(keccak256("x"), hex"1234") != MAGIC);
    }

    /// @dev A Safe-owned account: the owner contract's own ERC-1271 decides.
    function test_aContractOwnerSignsThroughItsOwnErc1271() public virtual {
        ContractOwner safeLike = new ContractOwner();
        vm.prank(address(safeLike));
        AgentAccount a = AgentAccount(
            payable(factory.createAccount(
                    address(safeLike),
                    "contract-owned",
                    AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true})
                ))
        );
        bytes32 h = keccak256("registry digest");
        assertTrue(a.isValidSignature(h, "") != MAGIC, "not approved yet");
        safeLike.approve(a.accountMessageHash(h));
        assertEq(a.isValidSignature(h, ""), MAGIC);
        assertTrue(a.isValidSignature(keccak256("other"), "") != MAGIC);
    }

    // ─────────── audit F1 / F4 (2026-10-26): the session manager ───────────
    // The renewer used to be the OWNER of every hosted account, because only
    // the owner could grant: a compromised signer could sweep, and every
    // re-grant reset `spent`, so a stolen key's "lifetime" budget refilled
    // each day. The manager can only push out the expiry of a key the owner
    // granted; `spent` and the budget stay.

    address internal manager = makeAddr("sessionManager");

    function _withManager() internal {
        vm.prank(agentOwner);
        account.setSessionManager(manager);
    }

    function test_onlyTheOwnerNamesTheSessionManager() public {
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setSessionManager(attacker);
        _withManager();
        assertEq(account.sessionManager(), manager);
    }

    function test_theManagerExtendsAKeyAndKeepsWhatItSpent() public {
        _withManager();
        vm.prank(sessionKey);
        account.execute(
            address(escrow),
            abi.encodeCall(TaskEscrow.directPay, (accountAgentId, workerId, MICRO, keccak256("m")))
        );
        (, uint128 budgetBefore, uint128 spentBefore) = account.sessionKeys(sessionKey);
        assertGt(spentBefore, 0);

        vm.warp(block.timestamp + 18 hours);
        uint64 later = uint64(block.timestamp + 1 days);
        vm.prank(manager);
        account.extendSessionKey(sessionKey, later);

        (uint64 expiry, uint128 budget, uint128 spent) = account.sessionKeys(sessionKey);
        assertEq(expiry, later);
        assertEq(budget, budgetBefore, "the budget is not raised");
        assertEq(spent, spentBefore, "what was spent stays spent");
    }

    function test_aStolenKeyCannotRefillItsBudgetThroughRenewal() public {
        _withManager();
        // spend the whole lifetime budget in daily-cap-sized pieces
        vm.prank(agentOwner);
        account.grantSessionKey(sessionKey, uint64(block.timestamp + 1 days), MICRO);
        vm.prank(sessionKey);
        account.execute(
            address(escrow),
            abi.encodeCall(TaskEscrow.directPay, (accountAgentId, workerId, MICRO, keccak256("a")))
        );

        vm.warp(block.timestamp + 18 hours);
        vm.prank(manager);
        account.extendSessionKey(sessionKey, uint64(block.timestamp + 1 days));

        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.SessionBudgetExceeded.selector);
        account.execute(
            address(escrow),
            abi.encodeCall(TaskEscrow.directPay, (accountAgentId, workerId, MICRO, keccak256("b")))
        );
    }

    function test_theManagerHasNoOwnerPower() public {
        _withManager();
        vm.startPrank(manager);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweep(manager, 1);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.grantSessionKey(manager, uint64(block.timestamp + 1 days), type(uint128).max);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setPolicy(
            AgentAccountCore.Policy({
                perTaskCap: type(uint128).max, dailyCap: type(uint128).max, allowlistOnly: false
            })
        );
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setAllowance(manager, type(uint256).max);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.setAllowedCall(address(token), IERC20.transfer.selector, true);
        vm.stopPrank();
    }

    function test_theManagerCannotCreateOrReviveAKey() public {
        _withManager();
        vm.prank(manager);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.extendSessionKey(manager, uint64(block.timestamp + 1 days));

        vm.prank(agentOwner);
        account.revokeSessionKey(sessionKey);
        vm.prank(manager);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.extendSessionKey(sessionKey, uint64(block.timestamp + 1 days));
    }

    function test_anExtensionKeepsTheTtlCeiling() public {
        _withManager();
        uint64 tooLate = uint64(block.timestamp + 1 days + 1);
        vm.prank(manager);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.SessionKeyTtlInvalid.selector,
                tooLate,
                uint64(block.timestamp + 1 days)
            )
        );
        account.extendSessionKey(sessionKey, tooLate);
    }

    function test_nobodyElseExtends() public {
        _withManager();
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.extendSessionKey(sessionKey, uint64(block.timestamp + 1 days));
    }

    function test_claimOwedIsNeverCallableThroughExecute() public {
        // audit I3: owed funds never enter the wallet, so a session key that
        // could call claimOwed would send them anywhere, outside every cap
        vm.prank(agentOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                AgentAccountCore.ForbiddenSelector.selector, TaskEscrow.claimOwed.selector
            )
        );
        account.setAllowedCall(address(escrow), TaskEscrow.claimOwed.selector, true);
    }

    /// @dev Audit F4: a new owner must not inherit the old owner's hot keys
    ///      or session manager.
    function test_aNewOwnerInheritsNoSessionKeysOrManager() public virtual {
        _withManager();
        address next = makeAddr("nextOwner");
        vm.prank(agentOwner);
        account.transferOwnership(next);
        vm.prank(next);
        account.acceptOwnership();

        (uint64 expiry,,) = account.sessionKeys(sessionKey);
        assertEq(expiry, 0, "the old grant is gone");
        assertEq(account.sessionManager(), address(0));
        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.execute(
            address(escrow),
            abi.encodeCall(TaskEscrow.directPay, (accountAgentId, workerId, MICRO, keccak256("x")))
        );
        vm.prank(manager);
        vm.expectRevert(AgentAccountCore.NotAuthorized.selector);
        account.extendSessionKey(sessionKey, uint64(block.timestamp + 1 days));

        // and the new owner can grant afresh
        vm.prank(next);
        account.grantSessionKey(sessionKey, uint64(block.timestamp + 1 days), MICRO);
        (expiry,,) = account.sessionKeys(sessionKey);
        assertGt(expiry, 0);
    }
}
