// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ClientReputationWriter} from "../../src/ClientReputationWriter.sol";
import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IReputationRegistry} from "../../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {BaseTest} from "../Base.t.sol";
import {ERC20} from "openzeppelin-contracts/token/ERC20/ERC20.sol";

/// @dev A token that starts taking a cut in transit only when told to — an
///      upgradeable stablecoin that changes its terms after a job was agreed.
contract ToggleFeeToken is ERC20 {
    bool public feeOn;

    constructor() ERC20("Toggle", "TOG") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function setFee(bool on) external {
        feeOn = on;
    }

    function _update(address from, address to, uint256 value) internal override {
        if (!feeOn || from == address(0) || to == address(0)) {
            super._update(from, to, value);
            return;
        }
        uint256 fee = value / 100;
        super._update(from, to, value - fee);
        super._update(from, address(0xdead), fee);
    }
}

/// @dev v3 branches no lifecycle test reaches on its own.
contract V3CoverageGapsTest is BaseTest {
    /// @dev The dispute bond gets the same fee-on-transfer guard as the job:
    ///      a bond that arrives short would break solvency (I1).
    function test_aDisputeBondThatArrivesShortIsRefused() public {
        ToggleFeeToken t = new ToggleFeeToken();
        vm.startPrank(admin);
        StakeVault v = new StakeVault(admin, ADMIN_DELAY);
        v.configure(address(t), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient);
        TaskEscrow e = new TaskEscrow(admin, ADMIN_DELAY);
        e.configure(
            address(t), address(identity), address(reputation), address(v), feeRecipient, _config()
        );
        v.addLocker(address(e));
        vm.stopPrank();

        t.mint(workerOwner, MIN_STAKE);
        vm.startPrank(workerOwner);
        t.approve(address(v), MIN_STAKE);
        v.deposit(workerId, MIN_STAKE);
        vm.stopPrank();
        t.mint(clientWallet, 1_000_000);
        vm.prank(clientWallet);
        t.approve(address(e), type(uint256).max);

        vm.prank(clientWallet);
        uint256 jobId =
            e.createJob(clientId, workerId, PRICE, keccak256("s"), ACCEPT_WINDOW, WORK_WINDOW);
        vm.startPrank(workerWallet);
        e.acceptJob(jobId);
        e.submitResult(jobId, keccak256("r"), "ipfs://r");
        vm.stopPrank();

        t.setFee(true);
        uint128 bond = e.getJob(jobId).disputeBond;
        vm.prank(clientWallet);
        vm.expectRevert(
            abi.encodeWithSelector(TaskEscrow.TokenDeliveredLess.selector, bond, bond - bond / 100)
        );
        e.dispute(jobId, keccak256("bad"));
    }

    function test_aFreeDisputeResolvesWithNoBondToMove() public {
        TaskEscrow.Config memory c = _config();
        c.disputeBondBps = 0;
        c.minDisputeBond = 0;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        uint256 jobId = _submittedJob();
        vm.prank(clientWallet);
        escrow.dispute(jobId, keccak256("bad"));
        vm.prank(admin);
        escrow.resolveDispute(jobId, true);
        assertEq(escrow.disputeBondTotal(), 0);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
    }

    /// @dev A registry that refuses the client review degrades to an event,
    ///      exactly as a refused worker review does (I8).
    function test_aRefusedClientReviewDoesNotBlockSettlement() public {
        ClientReputationWriter w =
            new ClientReputationWriter(address(escrow), IReputationRegistry(address(reputation)));
        vm.prank(admin);
        escrow.setClientWriter(address(w));
        uint256 jobId = _submittedJob();
        reputation.setShouldRevert(true);

        vm.expectEmit(true, true, false, false, address(escrow));
        emit ITaskEscrow.FeedbackFailed(jobId, clientId);
        vm.prank(clientWallet);
        escrow.approve(jobId);
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
    }

    /// @dev With bonds switched off nothing is locked, and an undelivered job
    ///      refunds the job alone.
    function test_withNoBondAnUndeliveredJobRefundsTheJobAlone() public {
        TaskEscrow.Config memory c = _config();
        c.bondBps = 0;
        c.minBond = 0;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _acceptedJob();
        assertEq(vault.lockedOf(workerId), 0);
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        assertEq(token.balanceOf(clientWallet), before);
    }

    /// @dev A slash that took the locked stake first leaves nothing to seize;
    ///      the refund still goes through.
    function test_aBondAlreadySlashedSeizesNothing() public {
        uint256 before = token.balanceOf(clientWallet);
        uint256 jobId = _acceptedJob();
        bytes32 slasher = vault.SLASHER_ROLE();
        vm.prank(admin);
        vault.grantRole(slasher, admin);
        vm.prank(admin);
        vault.slash(workerId, MIN_STAKE, "ruling");
        vm.warp(block.timestamp + ACCEPT_WINDOW + WORK_WINDOW + 1);
        escrow.expireUndelivered(jobId);
        assertEq(token.balanceOf(clientWallet), before, "refund only");
        assertEq(vault.lockedOf(workerId), 0);
    }

    function test_theWriterRefusesZeroAddresses() public {
        vm.expectRevert(ClientReputationWriter.ZeroAddress.selector);
        new ClientReputationWriter(address(0), IReputationRegistry(address(reputation)));
        vm.expectRevert(ClientReputationWriter.ZeroAddress.selector);
        new ClientReputationWriter(address(escrow), IReputationRegistry(address(0)));
    }
}
