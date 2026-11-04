// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccountFactory} from "../src/AgentAccountFactory.sol";
import {AgentDelegate} from "../src/AgentDelegate.sol";
import {ClientReputationWriter} from "../src/ClientReputationWriter.sol";
import {SettlementValidator} from "../src/SettlementValidator.sol";
import {StakeVault} from "../src/StakeVault.sol";
import {TaskEscrow} from "../src/TaskEscrow.sol";
import {IReputationRegistry, IValidationRegistry} from "../src/interfaces/IERC8004.sol";
import {ITaskEscrow} from "../src/interfaces/ITaskEscrow.sol";
import {MockIdentityRegistry, MockReputationRegistry} from "../src/mocks/MockERC8004.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @title Deploy — network-agnostic deployment driven entirely by config/
/// @notice One script for every network. The chain is selected by `--rpc-url`,
///         and everything else is read from:
///           config/networks.json        what this chain is
///           config/params.<chainId>.json what the protocol parameters are
///
///         There is no `if (mainnet)` anywhere. See docs/08.
///
/// @dev Deploys the full protocol: payment token (where configured), the
///      ERC-8004 registries on chains where they are absent (Monad testnet —
///      verified 2026-09-22, see docs/09 §8), then StakeVault, TaskEscrow and
///      AgentAccountFactory, wired together from config.
contract Deploy is Script {
    struct Params {
        uint96 minStake;
        uint64 withdrawDelaySeconds;
        uint128 fastPathMax;
        uint128 minJobAmount;
        uint128 minFee;
        uint16 protocolFeeBps;
        uint64 acceptWindowSeconds;
        uint64 workWindowSeconds;
        uint64 reviewWindowSeconds;
        uint64 disputeTimeoutSeconds;
        uint48 adminDelaySeconds;
        uint128 maxJobAmount;
        uint32 feedbackGas;
        uint16 bondBps;
        uint128 minBond;
        uint16 disputeBondBps;
        uint128 minDisputeBond;
    }

    string internal networksJson;
    string internal paramsJson;
    uint256 internal chainId;

    function setUp() public {
        chainId = block.chainid;
        networksJson = vm.readFile("config/networks.json");
        paramsJson = vm.readFile(string.concat("config/params.", vm.toString(chainId), ".json"));
    }

    function run() public {
        Params memory p = _readParams();
        bool isTestnet = _netBool("testnet");
        address admin = msg.sender;
        // Both default to the deployer. `.env.example` has documented these
        // two variables from the start, and until now nothing read them.
        // On testnet they default to the deployer. Off testnet each must be
        // named and be a contract (a Safe): forgetting one variable must not
        // leave real fees or a dispute vote with a hot key (v3).
        address arbiter = _roleHolder("ARBITER_ADDRESS", admin, isTestnet);
        address feeRecipient = _roleHolder("FEE_RECIPIENT", admin, isTestnet);
        // Slashed bonds go here; the fee recipient unless told otherwise.
        address slashRecipient = _roleHolder("SLASH_RECIPIENT", feeRecipient, isTestnet);
        // May pause new jobs, nothing else; the admin keeps it too (v3).
        address pauser = _roleHolder("PAUSER_ADDRESS", admin, isTestnet);

        console2.log("=====================================");
        console2.log("AGENTX deploy");
        console2.log("  chain    :", _netString("name"));
        console2.log("  chainId  :", chainId);
        console2.log("  testnet  :", isTestnet);
        console2.log("  admin    :", admin);
        console2.log("  fees to  :", feeRecipient);
        console2.log("  arbiter  :", arbiter);
        console2.log("  slash to :", slashRecipient);
        console2.log("  pauser   :", pauser);
        console2.log("=====================================");

        vm.startBroadcast();

        address paymentToken = _resolvePaymentToken(isTestnet);
        (address identityRegistry, address reputationRegistry) = _resolveErc8004(isTestnet);

        // The admin delay makes handing over the admin role a two-step,
        // time-locked operation. Plain CREATE: the delay differs per chain, so
        // the constructor arguments (and the addresses) differ too.
        StakeVault vault = new StakeVault(admin, p.adminDelaySeconds);
        vault.configure(
            paymentToken, identityRegistry, p.minStake, p.withdrawDelaySeconds, slashRecipient
        );

        TaskEscrow escrow = new TaskEscrow(admin, p.adminDelaySeconds);
        escrow.configure(
            paymentToken,
            identityRegistry,
            reputationRegistry,
            address(vault),
            feeRecipient,
            TaskEscrow.Config({
                fastPathMax: p.fastPathMax,
                minJobAmount: p.minJobAmount,
                minFee: p.minFee,
                protocolFeeBps: p.protocolFeeBps,
                reviewWindow: p.reviewWindowSeconds,
                // Caller-supplied windows are clamped to these. Without a
                // ceiling a client could set a 1000-year deadline and lock its
                // own funds past any refund.
                maxAcceptWindow: p.acceptWindowSeconds * 10,
                maxWorkWindow: p.workWindowSeconds * 10,
                disputeTimeout: p.disputeTimeoutSeconds,
                maxJobAmount: p.maxJobAmount,
                feedbackGas: p.feedbackGas,
                bondBps: p.bondBps,
                minBond: p.minBond,
                disputeBondBps: p.disputeBondBps,
                minDisputeBond: p.minDisputeBond
            })
        );
        // The escrow locks a share of the worker's bond per job (v3).
        vault.addLocker(address(escrow));
        // Clients are rated from their own address, never mixed with workers (v3).
        ClientReputationWriter clientWriter =
            new ClientReputationWriter(address(escrow), IReputationRegistry(reputationRegistry));
        escrow.setClientWriter(address(clientWriter));

        // The constructor makes the admin the arbiter. A separate arbiter
        // replaces it rather than joining it: two parties able to decide a
        // dispute is one more than the design says.
        if (arbiter != admin) {
            escrow.grantRole(escrow.ARBITER_ROLE(), arbiter);
            escrow.revokeRole(escrow.ARBITER_ROLE(), admin);
        }

        if (pauser != admin) escrow.grantRole(escrow.PAUSER_ROLE(), pauser);

        AgentAccountFactory factory = new AgentAccountFactory(paymentToken);
        // v3: the same spend controls for an EOA that keeps its funds, by
        // EIP-7702 delegation. Stateless; one per chain is enough.
        AgentDelegate delegate = new AgentDelegate();

        // v3: where the chain has an ERC-8004 Validation Registry, answer
        // validation requests for this escrow's jobs from its own state.
        address validator;
        if (vm.keyExistsJson(networksJson, _key("erc8004.validationRegistry"))) {
            address registry = _netAddress("erc8004.validationRegistry");
            require(registry.code.length > 0, "erc8004.validationRegistry has no code");
            validator = address(
                new SettlementValidator(ITaskEscrow(address(escrow)), IValidationRegistry(registry))
            );
        }

        vm.stopBroadcast();

        console2.log("");
        console2.log("DEPLOYED PaymentToken       :", paymentToken);
        console2.log("DEPLOYED IdentityRegistry   :", identityRegistry);
        console2.log("DEPLOYED ReputationRegistry :", reputationRegistry);
        console2.log("DEPLOYED StakeVault         :", address(vault));
        console2.log("DEPLOYED TaskEscrow         :", address(escrow));
        console2.log("DEPLOYED AccountFactory     :", address(factory));
        console2.log("DEPLOYED AgentDelegate      :", address(delegate));
        if (validator != address(0)) {
            console2.log("DEPLOYED SettlementValidator:", validator);
        }
        console2.log("");
        console2.log("PARAM minStake    :", p.minStake);
        console2.log("PARAM fastPathMax :", p.fastPathMax);
        console2.log("PARAM feeBps      :", p.protocolFeeBps);
        console2.log("PARAM minJob      :", p.minJobAmount);
        console2.log("PARAM minFee      :", p.minFee);
        console2.log("PARAM dispute (s) :", p.disputeTimeoutSeconds);
        console2.log("PARAM adminDelay  :", p.adminDelaySeconds);
        console2.log("");
        console2.log("Next: node scripts/write-deployment.mjs <network-alias>");
    }

    /// @dev Deploy a mintable test token, or adopt the canonical one — decided
    ///      by config, never by a branch in application code.
    function _resolvePaymentToken(bool isTestnet) internal returns (address) {
        bool deployToken = _netBool("paymentToken.deployWithProtocol");

        if (!deployToken) {
            address configured = _netAddress("paymentToken.address");
            require(configured != address(0), "paymentToken.address unset (see D8)");
            require(configured.code.length > 0, "paymentToken.address has no code");
            console2.log("paymentToken: using canonical", configured);
            return configured;
        }

        // A mintable token must never reach a production network. The config
        // checker enforces this too; belt and braces, because this one is
        // irreversible.
        require(isTestnet, "refusing to deploy MockUSDC on a non-testnet");

        MockUSDC token = new MockUSDC();
        console2.log("paymentToken: deployed MockUSDC", address(token));
        return address(token);
    }

    /// @dev ERC-8004 is live on Monad mainnet and ABSENT on testnet (verified
    ///      on-chain 2026-09-22). Rather than branch in application code, the
    ///      registries are deployed here where missing and adopted where
    ///      present — the rest of the system cannot tell the difference.
    function _resolveErc8004(bool isTestnet)
        internal
        returns (address identity, address reputation)
    {
        if (!_netBool("erc8004.deployWithProtocol")) {
            identity = _netAddress("erc8004.identityRegistry");
            reputation = _netAddress("erc8004.reputationRegistry");
            require(identity.code.length > 0, "erc8004.identityRegistry has no code");
            require(reputation.code.length > 0, "erc8004.reputationRegistry has no code");
            console2.log("erc8004: adopting the canonical registries");
            return (identity, reputation);
        }

        // These are the reference registries, not a canonical deployment.
        // Deploying them on a production chain would fragment the standard.
        require(isTestnet, "refusing to deploy ERC-8004 registries on a non-testnet");

        MockIdentityRegistry id = new MockIdentityRegistry();
        MockReputationRegistry rep = new MockReputationRegistry(id);
        console2.log("erc8004: deployed reference registries (absent on this chain)");
        return (address(id), address(rep));
    }

    // ── config readers ───────────────────────────────────────────────────
    // Amounts are strings in JSON so they survive any tooling that would
    // otherwise round them through a float.

    function _readParams() internal view returns (Params memory p) {
        p.minStake = uint96(vm.parseUint(vm.parseJsonString(paramsJson, ".minStake")));
        p.withdrawDelaySeconds = uint64(vm.parseJsonUint(paramsJson, ".withdrawDelaySeconds"));
        p.fastPathMax = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".fastPathMax")));
        p.minJobAmount = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".minJobAmount")));
        p.minFee = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".minFee")));
        p.protocolFeeBps = uint16(vm.parseJsonUint(paramsJson, ".protocolFeeBps"));
        p.acceptWindowSeconds = uint64(vm.parseJsonUint(paramsJson, ".acceptWindowSeconds"));
        p.workWindowSeconds = uint64(vm.parseJsonUint(paramsJson, ".workWindowSeconds"));
        p.reviewWindowSeconds = uint64(vm.parseJsonUint(paramsJson, ".reviewWindowSeconds"));
        p.disputeTimeoutSeconds = uint64(vm.parseJsonUint(paramsJson, ".disputeTimeoutSeconds"));
        p.maxJobAmount = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".maxJobAmount")));
        p.feedbackGas = uint32(vm.parseJsonUint(paramsJson, ".feedbackGas"));
        p.bondBps = uint16(vm.parseJsonUint(paramsJson, ".bondBps"));
        p.minBond = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".minBond")));
        p.disputeBondBps = uint16(vm.parseJsonUint(paramsJson, ".disputeBondBps"));
        p.minDisputeBond = uint128(vm.parseUint(vm.parseJsonString(paramsJson, ".minDisputeBond")));
        p.adminDelaySeconds = uint48(vm.parseJsonUint(paramsJson, ".adminDelaySeconds"));
        // fastPathMinScore and confidenceFloor are OFF-chain policy, enforced
        // by the API; the contract never read them, so it no longer stores them.

        require(p.fastPathMax > 0, "fastPathMax must be > 0");
        require(p.protocolFeeBps <= 1000, "protocolFeeBps > 10%");
        require(p.reviewWindowSeconds > 0, "reviewWindow 0 would trap funds");
        require(p.minFee < p.minJobAmount, "minFee must be below minJobAmount");
        require(p.disputeTimeoutSeconds > 0, "disputeTimeout 0 would expire every dispute");
    }

    function _key(string memory path) internal view returns (string memory) {
        return string.concat(".", vm.toString(chainId), ".", path);
    }

    function _netBool(string memory path) internal view returns (bool) {
        return vm.parseJsonBool(networksJson, _key(path));
    }

    function _netString(string memory path) internal view returns (string memory) {
        return vm.parseJsonString(networksJson, _key(path));
    }

    function _netAddress(string memory path) internal view returns (address) {
        return vm.parseJsonAddress(networksJson, _key(path));
    }

    /// @dev A role holder from the environment; see `_checkRoleHolder`.
    function _roleHolder(string memory name, address fallback_, bool isTestnet)
        internal
        view
        returns (address)
    {
        return _checkRoleHolder(name, _envAddressOr(name, address(0)), fallback_, isTestnet);
    }

    /// @dev Testnet: `fallback_` when unset. Elsewhere: required, with code.
    function _checkRoleHolder(string memory name, address holder, address fallback_, bool isTestnet)
        internal
        view
        returns (address)
    {
        if (isTestnet) return holder == address(0) ? fallback_ : holder;
        require(holder != address(0), string.concat(name, ": must be set to a Safe off testnet"));
        require(
            holder.code.length > 0, string.concat(name, ": must be a contract (a Safe) off testnet")
        );
        return holder;
    }

    /// @dev `.env` sets unused variables to the empty string, which
    ///      `vm.envOr(name, address)` cannot parse — so read a string and
    ///      treat empty as unset.
    function _envAddressOr(string memory name, address fallback_) internal view returns (address) {
        string memory raw = vm.envOr(name, string(""));
        if (bytes(raw).length == 0) return fallback_;
        return vm.parseAddress(raw);
    }
}
