// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// @title ERC-8004 Trustless Agents — the subset AGENTX depends on
/// @notice External registries. AGENTX does NOT implement these; it reads the
///         Identity Registry and writes to the Reputation Registry.
///
/// Deployments (verified on-chain 2026-09-22):
///   Monad mainnet 143    Identity   0x8004A169FB4a3325136EB29fA0ceB6D2e539a432
///                        Reputation 0x8004BAa17C55a88189AE136b182e5fdA19dE9b63
///   Monad testnet 10143  NOT DEPLOYED — we deploy the reference implementation.
///
/// @dev Both mainnet registries are ERC-1967 upgradeable proxies. Their ABI can
///      change without warning. Addresses are never hardcoded in this codebase:
///      they come from deployments/<chainId>.json, and CI pins the mainnet
///      implementation address.
///
/// The EIP is a DRAFT. These signatures are transcribed from it and MUST be
/// checked against the deployed bytecode before mainnet use (task M1-00b).

struct MetadataEntry {
    string metadataKey;
    bytes metadataValue;
}

interface IIdentityRegistry {
    event Registered(uint256 indexed agentId, string agentURI, address indexed owner);
    event URIUpdated(uint256 indexed agentId, string newURI, address indexed updatedBy);

    function register(string calldata agentURI, MetadataEntry[] calldata metadata)
        external
        returns (uint256 agentId);

    function register(string calldata agentURI) external returns (uint256 agentId);

    function setAgentURI(uint256 agentId, string calldata newURI) external;

    function setMetadata(uint256 agentId, string calldata metadataKey, bytes calldata metadataValue)
        external;

    function getMetadata(uint256 agentId, string calldata metadataKey)
        external
        view
        returns (bytes memory);

    /// @notice The address that receives payment for this agent.
    /// @dev AGENTX treats this as the agent's AgentAccount.
    function getAgentWallet(uint256 agentId) external view returns (address);

    // --- ERC-721 surface we rely on ---
    function ownerOf(uint256 agentId) external view returns (address);

    /// @notice True for the agent's owner and its approved operators — the
    ///         exact test the Reputation Registry uses to refuse self-feedback
    ///         (upstream IdentityRegistryUpgradeable, verified 2026-10-25).
    function isAuthorizedOrOwner(address spender, uint256 agentId) external view returns (bool);
}

interface IReputationRegistry {
    /// @notice Write a feedback signal about an agent.
    /// @dev CALLABLE BY ANYONE. That is the standard's design, and the reason a
    ///      published study found ERC-8004 reputation manipulable. AGENTX does
    ///      not try to change it — it makes its OWN feedback expensive to
    ///      produce (only TaskEscrow, only post-settlement) and relies on
    ///      getSummary()'s clientAddresses filter for readers to count only that.
    ///
    ///      UNVERIFIED (M1-00b): whether a CONTRACT may call this, or only an
    ///      EOA. The entire AGENTX differentiator rests on the former.
    function giveFeedback(
        uint256 agentId,
        int128 value,
        uint8 valueDecimals,
        string calldata tag1,
        string calldata tag2,
        string calldata endpoint,
        string calldata feedbackURI,
        bytes32 feedbackHash
    ) external;

    /// @notice Aggregate feedback, optionally restricted to specific writers.
    /// @param clientAddresses Empty = count everyone (worthless). Pass
    ///        [address(taskEscrow)] to count only settlement-backed feedback.
    function getSummary(
        uint256 agentId,
        address[] calldata clientAddresses,
        string calldata tag1,
        string calldata tag2
    ) external view returns (uint64 count, int128 summaryValue, uint8 summaryValueDecimals);

    function getIdentityRegistry() external view returns (address);
}

/// @dev The ERC-8004 Validation Registry, as deployed (v2.0.0): upstream
///      erc-8004/erc-8004-contracts `ValidationRegistryUpgradeable.sol` at
///      27caef9. Monad testnet 0x8004Cb1B…4272 matches all eight selectors.
///      Only the subset AGENTX calls. The spec marks this registry as still
///      under discussion, so it is read, never relied on for money.
interface IValidationRegistry {
    function validationRequest(
        address validatorAddress,
        uint256 agentId,
        string calldata requestURI,
        bytes32 requestHash
    ) external;

    function validationResponse(
        bytes32 requestHash,
        uint8 response,
        string calldata responseURI,
        bytes32 responseHash,
        string calldata tag
    ) external;

    /// @dev Reverts "unknown" for a hash nobody requested.
    function getValidationStatus(bytes32 requestHash)
        external
        view
        returns (
            address validatorAddress,
            uint256 agentId,
            uint8 response,
            bytes32 responseHash,
            string memory tag,
            uint256 lastUpdate
        );

    function getSummary(uint256 agentId, address[] calldata validatorAddresses, string calldata tag)
        external
        view
        returns (uint64 count, uint8 avgResponse);

    function getIdentityRegistry() external view returns (address);
}
