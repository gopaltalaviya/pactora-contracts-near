// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {StakeVault} from "../src/StakeVault.sol";
import {TaskEscrow} from "../src/TaskEscrow.sol";
import {ITaskEscrow} from "../src/interfaces/ITaskEscrow.sol";
import {MockIdentityRegistry, MockReputationRegistry} from "../src/mocks/MockERC8004.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {Test} from "forge-std/Test.sol";

/// @dev The escrow's parameters as one struct. The public getter returns them
///      as a tuple, and unpacking 14 fields into locals is too deep for the
///      low-optimisation build `forge coverage` uses; the tuple and the struct
///      share an ABI encoding, so decoding the raw return is exact.
function readConfig(TaskEscrow escrow_) view returns (TaskEscrow.Config memory c) {
    (bool ok, bytes memory ret) = address(escrow_).staticcall(abi.encodeCall(escrow_.config, ()));
    require(ok, "config() failed");
    c = abi.decode(ret, (TaskEscrow.Config));
}

/// @dev Shared fixture. Parameters mirror config/params.31337.json so the tests
///      exercise the values we actually deploy, not invented ones.
abstract contract BaseTest is Test {
    MockUSDC internal token;
    MockIdentityRegistry internal identity;
    MockReputationRegistry internal reputation;
    StakeVault internal vault;
    TaskEscrow internal escrow;

    address internal admin = makeAddr("admin");
    address internal feeRecipient = makeAddr("feeRecipient");
    address internal slashRecipient = makeAddr("slashRecipient");

    address internal clientOwner = makeAddr("clientOwner");
    address internal clientWallet = makeAddr("clientWallet");
    address internal workerOwner = makeAddr("workerOwner");
    address internal workerWallet = makeAddr("workerWallet");
    address internal stranger = makeAddr("stranger");

    uint256 internal clientId;
    uint256 internal workerId;

    // params.31337.json
    uint96 internal constant MIN_STAKE = 10_000_000; // 10 USDC
    uint64 internal constant WITHDRAW_DELAY = 7 days;
    uint128 internal constant FAST_PATH_MAX = 30_000; // 0.03 USDC
    uint16 internal constant FEE_BPS = 100; // 1%
    uint64 internal constant ACCEPT_WINDOW = 60;
    uint64 internal constant WORK_WINDOW = 120;
    uint64 internal constant REVIEW_WINDOW = 60;
    uint128 internal constant MIN_JOB_AMOUNT = 10_000; // 0.01 USDC
    uint128 internal constant MIN_FEE = 100; // 0.0001 USDC
    uint64 internal constant DISPUTE_TIMEOUT = 1 hours;
    uint128 internal constant MAX_JOB_AMOUNT = 10_000_000; // 10 USDC
    uint32 internal constant FEEDBACK_GAS = 500_000;
    uint16 internal constant BOND_BPS = 2_000; // 20% of the job
    uint128 internal constant MIN_BOND = 10_000; // 0.01 USDC
    uint16 internal constant DISPUTE_BOND_BPS = 1_000; // 10% of the job
    uint128 internal constant MIN_DISPUTE_BOND = 10_000; // 0.01 USDC
    uint48 internal constant ADMIN_DELAY = 1 days;

    uint128 internal constant PRICE = 50_000; // 0.05 USDC — escrow path
    uint128 internal constant MICRO = 20_000; // 0.02 USDC — fast path

    function setUp() public virtual {
        token = new MockUSDC();
        identity = new MockIdentityRegistry();
        reputation = new MockReputationRegistry(identity);

        vm.startPrank(admin);
        vault = new StakeVault(admin, ADMIN_DELAY);
        escrow = new TaskEscrow(admin, ADMIN_DELAY);

        vault.configure(
            address(token), address(identity), MIN_STAKE, WITHDRAW_DELAY, slashRecipient
        );
        escrow.configure(
            address(token),
            address(identity),
            address(reputation),
            address(vault),
            feeRecipient,
            _config()
        );
        vault.addLocker(address(escrow));
        vm.stopPrank();

        vm.prank(clientOwner);
        clientId = identity.register("ipfs://client", clientWallet);
        vm.prank(workerOwner);
        workerId = identity.register("ipfs://worker", workerWallet);

        _bond(workerId, workerOwner, MIN_STAKE);
        _bond(clientId, clientOwner, MIN_STAKE);

        token.mint(clientWallet, 1_000_000_000);
        vm.prank(clientWallet);
        token.approve(address(escrow), type(uint256).max);

        // Time must be past the windows so `block.timestamp - x` never underflows.
        vm.warp(1_700_000_000);
    }

    function _bond(uint256 agentId, address owner, uint96 amount) internal {
        token.mint(owner, amount);
        vm.startPrank(owner);
        token.approve(address(vault), amount);
        vault.deposit(agentId, amount);
        vm.stopPrank();
    }

    function _createJob() internal returns (uint256 jobId) {
        vm.prank(clientWallet);
        jobId = escrow.createJob(
            clientId, workerId, PRICE, keccak256("spec"), ACCEPT_WINDOW, WORK_WINDOW
        );
    }

    function _acceptedJob() internal returns (uint256 jobId) {
        jobId = _createJob();
        vm.prank(workerWallet);
        escrow.acceptJob(jobId);
    }

    function _submittedJob() internal returns (uint256 jobId) {
        jobId = _acceptedJob();
        vm.prank(workerWallet);
        escrow.submitResult(jobId, keccak256("result"), "ipfs://result");
    }

    /// @dev The deployed parameters, for tests to take and adjust one field of.
    function _config() internal pure returns (TaskEscrow.Config memory) {
        return TaskEscrow.Config({
            fastPathMax: FAST_PATH_MAX,
            minJobAmount: MIN_JOB_AMOUNT,
            minFee: MIN_FEE,
            protocolFeeBps: FEE_BPS,
            reviewWindow: REVIEW_WINDOW,
            maxAcceptWindow: 1 days,
            maxWorkWindow: 7 days,
            disputeTimeout: DISPUTE_TIMEOUT,
            maxJobAmount: MAX_JOB_AMOUNT,
            feedbackGas: FEEDBACK_GAS,
            bondBps: BOND_BPS,
            minBond: MIN_BOND,
            disputeBondBps: DISPUTE_BOND_BPS,
            minDisputeBond: MIN_DISPUTE_BOND
        });
    }

    /// @dev Lift the per-job ceiling, for tests about arithmetic at extreme
    ///      amounts rather than about the ceiling itself.
    function _liftMaxJobAmount() internal {
        TaskEscrow.Config memory c = _config();
        c.maxJobAmount = type(uint128).max;
        vm.prank(admin);
        escrow.setParams(c, feeRecipient);
    }

    function _cfgWith(uint128 fastPathMax, uint16 feeBps)
        internal
        pure
        returns (TaskEscrow.Config memory c)
    {
        c = _config();
        c.fastPathMax = fastPathMax;
        c.protocolFeeBps = feeBps;
    }

    function _feeOf(uint128 amount) internal pure returns (uint128 fee) {
        fee = uint128((uint256(amount) * FEE_BPS) / 10_000);
        if (fee < MIN_FEE) fee = MIN_FEE;
    }

    function _settledCount(uint256 agentId) internal view returns (uint64 count) {
        address[] memory clients = new address[](1);
        clients[0] = address(escrow);
        (count,,) = reputation.getSummary(agentId, clients, "agentx", "settled");
    }
}
