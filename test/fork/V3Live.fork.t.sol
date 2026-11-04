// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ISafe, ISafeProxyFactory, SafeAddresses} from "../../script/safe/ISafe.sol";
import {AgentAccount} from "../../src/AgentAccount.sol";
import {AgentAccountCore} from "../../src/AgentAccountCore.sol";
import {AgentAccountFactory} from "../../src/AgentAccountFactory.sol";
import {AgentDelegate} from "../../src/AgentDelegate.sol";
import {ClientReputationWriter} from "../../src/ClientReputationWriter.sol";
import {SettlementValidator} from "../../src/SettlementValidator.sol";
import {StakeVault} from "../../src/StakeVault.sol";
import {TaskEscrow} from "../../src/TaskEscrow.sol";
import {IReputationRegistry, IValidationRegistry} from "../../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../../src/interfaces/ITaskEscrow.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "openzeppelin-contracts/utils/cryptography/MessageHashUtils.sol";

interface ICanonicalIdentity {
    function register(string memory agentURI) external returns (uint256 agentId);
    function getAgentWallet(uint256 agentId) external view returns (address);
    function setAgentWallet(
        uint256 agentId,
        address newWallet,
        uint256 deadline,
        bytes calldata signature
    ) external;
    function eip712Domain()
        external
        view
        returns (bytes1, string memory, string memory, uint256, address, bytes32, uint256[] memory);
}

interface ICanonicalReputation {
    function getLastIndex(uint256 agentId, address clientAddress) external view returns (uint64);
    function readFeedback(uint256 agentId, address clientAddress, uint64 feedbackIndex)
        external
        view
        returns (
            int128 value,
            uint8 valueDecimals,
            string memory tag1,
            string memory tag2,
            bool isRevoked
        );
}

/// @dev v3 against Monad testnet's REAL contracts, on a local fork — nothing
///      is sent to the chain. The canonical ERC-8004 registries (verified
///      2026-10-25: identity 0x8004A818…, implementation 0x7274e874…, the one
///      pinned for mainnet; reputation 0x8004B663…, version 2.0.0) and Safe
///      v1.4.1. A mock that agrees with our assumptions proves nothing.
///
///      Runs only with FORK_RPC_10143 set, e.g.
///        FORK_RPC_10143=https://testnet-rpc.monad.xyz forge test --match-path "test/fork/*"
contract V3LiveForkTest is Test {
    ICanonicalIdentity internal constant IDENTITY =
        ICanonicalIdentity(0x8004A818BFB912233c491871b3d84c89A494BD9e);
    ICanonicalReputation internal constant REPUTATION =
        ICanonicalReputation(0x8004B663056A597Dffe9eCcC1965A193B7388713);
    /// @dev Version 2.0.0, implementation 0xdb31f5d9…7f99 (verified 2026-10-25).
    IValidationRegistry internal constant VALIDATION =
        IValidationRegistry(0x8004Cb1BF31DAf7788923b405b754f57acEB4272);

    MockUSDC internal token;
    StakeVault internal vault;
    TaskEscrow internal escrow;
    ClientReputationWriter internal writer;
    ISafe internal safe;

    address internal admin = makeAddr("admin");
    address internal client = makeAddr("client");
    address internal worker = makeAddr("worker");
    uint256[3] internal ownerKeys;
    address[3] internal owners;
    uint256 internal clientId;
    uint256 internal workerId;

    function setUp() public {
        string memory rpc = vm.envOr("FORK_RPC_10143", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);
        assertEq(block.chainid, 10_143, "not Monad testnet");

        // The arbiter: a 2-of-3 Safe, as the owner chose for testnet.
        for (uint256 i; i < 3; i++) {
            (owners[i], ownerKeys[i]) = makeAddrAndKey(string.concat("safeOwner", vm.toString(i)));
        }
        address[] memory list = new address[](3);
        for (uint256 i; i < 3; i++) {
            list[i] = owners[i];
        }
        safe = ISafe(
            ISafeProxyFactory(SafeAddresses.PROXY_FACTORY)
                .createProxyWithNonce(
                    SafeAddresses.SAFE_L2,
                    abi.encodeCall(
                        ISafe.setup,
                        (
                            list,
                            2,
                            address(0),
                            "",
                            SafeAddresses.FALLBACK_HANDLER,
                            address(0),
                            0,
                            payable(0)
                        )
                    ),
                    uint256(keccak256(abi.encode("fork", block.number)))
                )
        );

        token = new MockUSDC();
        vm.startPrank(admin);
        vault = new StakeVault(admin, 1 days);
        vault.configure(address(token), address(IDENTITY), 10_000_000, 7 days, admin);
        escrow = new TaskEscrow(admin, 1 days);
        escrow.configure(
            address(token),
            address(IDENTITY),
            address(REPUTATION),
            address(vault),
            admin,
            TaskEscrow.Config({
                fastPathMax: 30_000,
                minJobAmount: 10_000,
                minFee: 100,
                protocolFeeBps: 100,
                reviewWindow: 600,
                maxAcceptWindow: 3000,
                maxWorkWindow: 18_000,
                disputeTimeout: 3600,
                maxJobAmount: 10_000_000,
                feedbackGas: 500_000,
                bondBps: 2000,
                minBond: 10_000,
                disputeBondBps: 1000,
                minDisputeBond: 10_000
            })
        );
        vault.addLocker(address(escrow));
        writer =
            new ClientReputationWriter(address(escrow), IReputationRegistry(address(REPUTATION)));
        escrow.setClientWriter(address(writer));
        escrow.grantRole(escrow.ARBITER_ROLE(), address(safe));
        escrow.revokeRole(escrow.ARBITER_ROLE(), admin);
        vm.stopPrank();

        // Agents on the canonical registry: the wallet starts as the owner.
        vm.prank(client);
        clientId = IDENTITY.register("ipfs://client");
        vm.prank(worker);
        workerId = IDENTITY.register("ipfs://worker");
        assertEq(IDENTITY.getAgentWallet(workerId), worker);

        token.mint(worker, 10_000_000);
        vm.startPrank(worker);
        token.approve(address(vault), 10_000_000);
        vault.deposit(workerId, 10_000_000);
        vm.stopPrank();
        token.mint(client, 1_000_000);
        vm.prank(client);
        token.approve(address(escrow), type(uint256).max);
    }

    function _submitted(uint128 amount) internal returns (uint256 jobId) {
        vm.prank(client);
        jobId = escrow.createJob(clientId, workerId, amount, keccak256("spec"), 300, 1800);
        vm.startPrank(worker);
        escrow.acceptJob(jobId);
        escrow.submitResult(jobId, keccak256("result"), "ipfs://result");
        vm.stopPrank();
    }

    /// @dev Two owners sign; signatures go in ascending owner order.
    function _safeExec(address to, bytes memory data) internal {
        bytes32 h =
            safe.getTransactionHash(to, 0, data, 0, 0, 0, 0, address(0), address(0), safe.nonce());
        (uint256 k0, uint256 k1) = (ownerKeys[0], ownerKeys[1]);
        if (owners[0] > owners[1]) (k0, k1) = (k1, k0);
        (uint8 v0, bytes32 r0, bytes32 s0) = vm.sign(k0, h);
        (uint8 v1, bytes32 r1, bytes32 s1) = vm.sign(k1, h);
        bytes memory sigs = abi.encodePacked(r0, s0, v0, r1, s1, v1);
        assertTrue(safe.execTransaction(to, 0, data, 0, 0, 0, 0, address(0), payable(0), sigs));
    }

    function test_theCanonicalRegistryAcceptsTheEscrowsReviews() public {
        uint256 jobId = _submitted(50_000);
        vm.prank(client);
        escrow.approve(jobId);

        assertEq(
            REPUTATION.getLastIndex(workerId, address(escrow)),
            1,
            "worker review on the canonical registry"
        );
        (int128 value,, string memory tag1, string memory tag2,) =
            REPUTATION.readFeedback(workerId, address(escrow), 1);
        assertEq(value, 100);
        assertEq(tag1, "agentx");
        assertEq(tag2, "settled");
        assertEq(
            REPUTATION.getLastIndex(clientId, address(writer)), 1, "client review, separate writer"
        );
        (,,, string memory clientTag,) = REPUTATION.readFeedback(clientId, address(writer), 1);
        assertEq(clientTag, "client");
    }

    function test_theSafeRulesADisputeWithTwoSignatures() public {
        uint256 jobId = _submitted(50_000);
        vm.prank(client);
        escrow.dispute(jobId, keccak256("bad"));

        // One key is not enough.
        vm.prank(owners[0]);
        vm.expectRevert();
        escrow.resolveDispute(jobId, true);

        uint256 before = token.balanceOf(worker);
        _safeExec(address(escrow), abi.encodeCall(TaskEscrow.resolveDispute, (jobId, true)));
        assertEq(uint8(escrow.getJob(jobId).state), uint8(ITaskEscrow.JobState.SETTLED));
        assertEq(
            token.balanceOf(worker) - before, 50_000 - 500 + 10_000, "payment plus the dispute bond"
        );
        assertEq(escrow.clientRecordOf(clientId).disputesRejected, 1);
    }

    /// @dev The starvation fix against the real registry's real cost: no gas
    ///      limit settles the job without the review.
    function test_noGasLimitDropsTheReviewOnTheCanonicalRegistry() public {
        uint256 jobId = _submitted(50_000);
        bytes memory data = abi.encodeCall(TaskEscrow.approve, (jobId));
        bool succeeded;
        for (uint256 g = 100_000; g <= 2_000_000; g += 20_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(client);
            (bool ok,) = address(escrow).call{gas: g}(data);
            uint64 reviews = REPUTATION.getLastIndex(workerId, address(escrow));
            vm.revertToState(snap);
            assertFalse(ok && reviews == 0, "settled without the review");
            if (ok) succeeded = true;
        }
        assertTrue(succeeded);
    }

    /// @dev The canonical registry sets a wallet only with that wallet's
    ///      signature; for a contract it asks ERC-1271. This is the proof that
    ///      an AgentAccount — the on-chain caps — can be an agent's wallet on
    ///      the real registry, and then hire through the escrow as the client.
    function test_anAgentAccountBecomesTheWalletOnTheCanonicalRegistry() public {
        (address human, uint256 humanKey) = makeAddrAndKey("human-owner");
        AgentAccountFactory factory = new AgentAccountFactory(address(token));
        vm.prank(human);
        AgentAccount acct = AgentAccount(
            payable(factory.createAccount(
                    human,
                    "fork",
                    AgentAccountCore.Policy({
                        perTaskCap: 100_000, dailyCap: 250_000, allowlistOnly: true
                    })
                ))
        );
        vm.prank(human);
        uint256 id = IDENTITY.register("ipfs://capped-client");

        // The registry's own EIP-712 domain, read from it, not assumed.
        (, string memory name, string memory version, uint256 chainId, address verifying,,) =
            IDENTITY.eip712Domain();
        bytes32 domain = keccak256(
            abi.encode(
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
                ),
                keccak256(bytes(name)),
                keccak256(bytes(version)),
                chainId,
                verifying
            )
        );
        uint256 deadline = block.timestamp + 4 minutes;
        bytes32 structHash = keccak256(
            abi.encode(
                keccak256(
                    "AgentWalletSet(uint256 agentId,address newWallet,address owner,uint256 deadline)"
                ),
                id,
                address(acct),
                human,
                deadline
            )
        );
        bytes32 digest = MessageHashUtils.toTypedDataHash(domain, structHash);
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(humanKey, acct.accountMessageHash(digest));

        vm.prank(human);
        IDENTITY.setAgentWallet(id, address(acct), deadline, abi.encodePacked(r, s_, v));
        assertEq(IDENTITY.getAgentWallet(id), address(acct), "the account is the agent's wallet");

        // And it hires through the escrow, inside its caps.
        token.mint(address(acct), 1_000_000);
        vm.startPrank(human);
        acct.setAllowedCall(address(escrow), TaskEscrow.createJob.selector, true);
        acct.setAllowance(address(escrow), type(uint256).max);
        bytes memory ret = acct.execute(
            address(escrow),
            abi.encodeCall(TaskEscrow.createJob, (id, workerId, 50_000, keccak256("s"), 300, 1800))
        );
        vm.stopPrank();
        uint256 jobId = abi.decode(ret, (uint256));
        assertEq(escrow.getJob(jobId).clientAgentId, id);
        assertEq(acct.spentToday(), 50_000, "the hire counted against the rolling cap");
    }

    function test_theDeployedValidationRegistryTakesTheEscrowsAnswer() public {
        SettlementValidator validator = new SettlementValidator(escrow, VALIDATION);
        uint256 jobId = _submitted(50_000);
        vm.prank(client);
        escrow.approve(jobId);

        bytes32 salt = keccak256("fork");
        bytes32 h = validator.requestHashOf(jobId, salt);
        // On the canonical registry the worker's address is also its owner.
        vm.prank(worker);
        VALIDATION.validationRequest(address(validator), workerId, "", h);
        assertEq(validator.respond(jobId, salt), 100);

        (address v, uint256 agentId, uint8 response,, string memory tag,) =
            VALIDATION.getValidationStatus(h);
        assertEq(v, address(validator));
        assertEq(agentId, workerId);
        assertEq(response, 100);
        assertEq(tag, "agentx-settlement");
        address[] memory only = new address[](1);
        only[0] = address(validator);
        (uint64 count, uint8 avg) = VALIDATION.getSummary(workerId, only, "agentx-settlement");
        assertEq(count, 1);
        assertEq(avg, 100);

        // A stranger cannot ask on the worker's behalf: the deployed rule.
        vm.prank(makeAddr("stranger"));
        vm.expectRevert();
        VALIDATION.validationRequest(address(validator), workerId, "", keccak256("x"));
    }

    /// @dev Found on testnet: the canonical identity registry mints with
    ///      _safeMint, so a delegated EOA (which has code) must answer
    ///      onERC721Received — it could not register until it did.
    function test_aDelegatedEoaRegistersOnTheCanonicalRegistry() public {
        address eoa = makeAddr("delegated-eoa");
        vm.etch(eoa, address(new AgentDelegate()).code);
        vm.prank(eoa);
        uint256 id = IDENTITY.register("ipfs://delegated");
        assertEq(IDENTITY.getAgentWallet(id), eoa, "it is its own wallet");
    }
}
