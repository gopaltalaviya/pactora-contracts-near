// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// @title Minimal ERC-8004 registries for tests and for testnet deployment
/// @notice Mirrors the behaviour of `erc-8004/erc-8004-contracts@master` that
///         AGENTX actually depends on — including the two constraints found by
///         reading that source on 2026-09-22:
///
///           1. `giveFeedback` is plain external (a CONTRACT may call it), but
///              reverts if the caller is the agent's owner or operator.
///           2. `getSummary` REVERTS on an empty `clientAddresses` list — there
///              is no such thing as an unfiltered ERC-8004 score.
///
///         `setShouldRevert(true)` exists so tests can prove invariant I8:
///         a reverting registry must never be able to revert a settlement.
contract MockIdentityRegistry {
    mapping(uint256 => address) internal _owner;
    mapping(uint256 => address) internal _wallet;
    mapping(uint256 => mapping(address => bool)) internal _operator;
    mapping(uint256 => mapping(string => bytes)) internal _metadata;
    uint256 public nextId = 1;
    /// @notice When set, `getAgentWallet` reverts — an upgraded or broken
    ///         registry. Used to prove a settlement can never be trapped by it.
    bool public walletShouldRevert;

    event Registered(uint256 indexed agentId, string agentURI, address indexed owner);

    function register(string calldata agentURI, address wallet) external returns (uint256 agentId) {
        agentId = nextId++;
        _owner[agentId] = msg.sender;
        _wallet[agentId] = wallet;
        emit Registered(agentId, agentURI, msg.sender);
    }

    /// @dev Lets tests mint a specific id, including ids above type(uint64).max
    ///      so the checked downcast in TaskEscrow can actually be exercised.
    function registerAt(uint256 agentId, address owner_, address wallet) external {
        require(_owner[agentId] == address(0), "taken");
        _owner[agentId] = owner_;
        _wallet[agentId] = wallet;
    }

    function ownerOf(uint256 agentId) external view returns (address) {
        address o = _owner[agentId];
        require(o != address(0), "ERC721NonexistentToken");
        return o;
    }

    function getAgentWallet(uint256 agentId) external view returns (address) {
        require(!walletShouldRevert, "registry unavailable");
        return _wallet[agentId];
    }

    function setWalletShouldRevert(bool v) external {
        walletShouldRevert = v;
    }

    function setAgentWallet(uint256 agentId, address wallet) external {
        require(_owner[agentId] == msg.sender, "not owner");
        _wallet[agentId] = wallet;
    }

    function setApprovalFor(uint256 agentId, address op, bool ok) external {
        _operator[agentId][op] = ok;
    }

    /// @dev The guard that produces threat T17.
    function isAuthorizedOrOwner(address who, uint256 agentId) external view returns (bool) {
        return _owner[agentId] == who || _operator[agentId][who];
    }

    function setMetadata(uint256 agentId, string calldata key, bytes calldata value) external {
        require(_owner[agentId] == msg.sender, "not owner");
        _metadata[agentId][key] = value;
    }

    function getMetadata(uint256 agentId, string calldata key)
        external
        view
        returns (bytes memory)
    {
        return _metadata[agentId][key];
    }
}

contract MockReputationRegistry {
    struct Feedback {
        int128 value;
        uint8 valueDecimals;
        string tag1;
        string tag2;
        bool isRevoked;
    }

    int128 private constant MAX_ABS_VALUE = 1e38;

    MockIdentityRegistry public immutable identityRegistry;
    bool public shouldRevert;

    mapping(uint256 => mapping(address => uint64)) internal _lastIndex;
    mapping(uint256 => mapping(address => mapping(uint64 => Feedback))) internal _feedback;

    event NewFeedback(
        uint256 indexed agentId,
        address indexed clientAddress,
        uint64 feedbackIndex,
        int128 value,
        uint8 valueDecimals,
        string indexed indexedTag1,
        string tag1,
        string tag2,
        string endpoint,
        string feedbackURI,
        bytes32 feedbackHash
    );

    constructor(MockIdentityRegistry registry) {
        identityRegistry = registry;
    }

    /// @notice Force every write to fail — used to prove invariant I8.
    function setShouldRevert(bool v) external {
        shouldRevert = v;
    }

    function giveFeedback(
        uint256 agentId,
        int128 value,
        uint8 valueDecimals,
        string calldata tag1,
        string calldata tag2,
        string calldata endpoint,
        string calldata feedbackURI,
        bytes32 feedbackHash
    ) external {
        require(!shouldRevert, "registry unavailable");
        require(valueDecimals <= 18, "too many decimals");
        require(value >= -MAX_ABS_VALUE && value <= MAX_ABS_VALUE, "value too large");
        require(
            !identityRegistry.isAuthorizedOrOwner(msg.sender, agentId), "Self-feedback not allowed"
        );

        uint64 idx = ++_lastIndex[agentId][msg.sender];
        _feedback[agentId][msg.sender][idx] = Feedback({
            value: value, valueDecimals: valueDecimals, tag1: tag1, tag2: tag2, isRevoked: false
        });

        emit NewFeedback(
            agentId,
            msg.sender,
            idx,
            value,
            valueDecimals,
            tag1,
            tag1,
            tag2,
            endpoint,
            feedbackURI,
            feedbackHash
        );
    }

    function getSummary(
        uint256 agentId,
        address[] calldata clientAddresses,
        string calldata tag1,
        string calldata tag2
    ) external view returns (uint64 count, int128 summaryValue, uint8 summaryValueDecimals) {
        // Matches upstream: naming your sources is mandatory.
        require(clientAddresses.length > 0, "clientAddresses required");

        bytes32 emptyHash = keccak256(bytes(""));
        bytes32 t1 = keccak256(bytes(tag1));
        bytes32 t2 = keccak256(bytes(tag2));
        int256 sum;

        for (uint256 i; i < clientAddresses.length; i++) {
            uint64 last = _lastIndex[agentId][clientAddresses[i]];
            for (uint64 j = 1; j <= last; j++) {
                Feedback storage fb = _feedback[agentId][clientAddresses[i]][j];
                if (fb.isRevoked) continue;
                if (t1 != emptyHash && keccak256(bytes(fb.tag1)) != t1) continue;
                if (t2 != emptyHash && keccak256(bytes(fb.tag2)) != t2) continue;
                sum += fb.value;
                count++;
            }
        }

        // Safe: a mean of int128 values is within int128.
        // forge-lint: disable-next-line(unsafe-typecast)
        summaryValue = count == 0 ? int128(0) : int128(sum / int256(uint256(count)));
        summaryValueDecimals = 0;
    }

    function getLastIndex(uint256 agentId, address client) external view returns (uint64) {
        return _lastIndex[agentId][client];
    }
}
