// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccountCore} from "./AgentAccountCore.sol";
import {IERC1271} from "openzeppelin-contracts/interfaces/IERC1271.sol";
import {IERC1155Receiver} from "openzeppelin-contracts/token/ERC1155/IERC1155Receiver.sol";
import {IERC721Receiver} from "openzeppelin-contracts/token/ERC721/IERC721Receiver.sol";
import {ECDSA} from "openzeppelin-contracts/utils/cryptography/ECDSA.sol";
import {IERC165} from "openzeppelin-contracts/utils/introspection/IERC165.sol";

/// @title AgentDelegate — AgentAccount's spend controls for an existing EOA,
///        by EIP-7702 delegation
///
/// @notice An EOA delegates to this contract and keeps its address, its funds
///         and its key; it gains session keys whose spending is capped on
///         chain, exactly as in `AgentAccount`. Nothing moves into a new
///         wallet. Verified on Monad testnet (2026-10-25): type-4
///         transactions are accepted and delegated code runs.
///
///         What the delegation does NOT do: bind the EOA's own key. That key
///         is the owner and can always act directly — as an AgentAccount's
///         owner can always `sweep`. The caps bind the AGENT's session keys,
///         which is what they are for. An EOA that wants its own key bound
///         too should use an AgentAccount owned by a multisig instead.
///
/// @dev Three differences from AgentAccount, each because of EIP-7702:
///      - The owner is the EOA itself, `address(this)`, and cannot change: the
///        key IS the owner, so there is no transfer to make. Owner calls are
///        the EOA calling itself.
///      - `initialize` is callable by the EOA alone. A freshly delegated EOA
///        has no state in this contract's namespace, and an open initializer
///        would let anyone who saw the delegation name the token and caps —
///        or, in a wallet with an owner field, take it. Nothing to front-run.
///      - ERC-1271 recovers the signer directly. Asking `SignatureChecker`
///        would, for an address that now has code, call `isValidSignature` on
///        itself, and recurse until it ran out of gas.
///
///      All state is in AgentAccountCore's ERC-7201 namespace, so whatever an
///      earlier delegate left at slot 0, 1, 2… is never read as ours.
///
///      And one thing an EOA could always do, kept: accept SAFE token
///      transfers. A delegated EOA has code, so `_safeMint` and
///      `safeTransferFrom` ask it for a receiver hook. Without one it could not
///      even register its own ERC-8004 identity — the canonical registry mints
///      with `_safeMint` (found on testnet, ERC721InvalidReceiver).
contract AgentDelegate is AgentAccountCore, IERC721Receiver, IERC1155Receiver {
    error AlreadyInitialized();

    /// @notice Set the token and caps. The EOA only, once.
    /// @dev Call it in the same type-4 transaction that sets the delegation
    ///      (the EOA sending to itself), or in any later self-call.
    /// @param token_ the token whose outflow the caps measure
    /// @param policy_ the caps
    function initialize(address token_, Policy calldata policy_) external {
        if (msg.sender != address(this)) revert NotOwner();
        Layout storage s = _layout();
        if (s.initialized) revert AlreadyInitialized();
        s.initialized = true;
        _init(address(this), token_, policy_);
    }

    /// @inheritdoc AgentAccountCore
    function owner() public view override returns (address) {
        return address(this);
    }

    function _domainName() internal pure override returns (string memory) {
        return "AgentDelegate";
    }

    /// @notice ERC-1271: valid when the EOA's own key signed
    ///         `accountMessageHash(hash)`. A session key's never counts.
    /// @param hash the digest being checked
    /// @param signature a 65-byte ECDSA signature by the EOA's key
    /// @return magic `0x1626ba7e` if valid, `0xffffffff` otherwise; never reverts
    function isValidSignature(bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4 magic)
    {
        // The third value is the error's detail, meaningful only when `err` is not
        // NoError — and every error is an invalid signature here.
        // slither-disable-next-line unused-return
        (address signer, ECDSA.RecoverError err,) =
            ECDSA.tryRecover(accountMessageHash(hash), signature);
        if (err == ECDSA.RecoverError.NoError && signer == address(this)) {
            return IERC1271.isValidSignature.selector;
        }
        return 0xffffffff;
    }

    // ─────────────── what a plain EOA accepted, still accepted ───────────────

    /// @notice Accept any ERC-721 safe transfer, as a plain EOA does.
    function onERC721Received(address, address, uint256, bytes calldata)
        external
        pure
        returns (bytes4)
    {
        return IERC721Receiver.onERC721Received.selector;
    }

    /// @notice Accept any ERC-1155 transfer, as a plain EOA does.
    function onERC1155Received(address, address, uint256, uint256, bytes calldata)
        external
        pure
        returns (bytes4)
    {
        return IERC1155Receiver.onERC1155Received.selector;
    }

    /// @notice Accept any ERC-1155 batch transfer, as a plain EOA does.
    function onERC1155BatchReceived(
        address,
        address,
        uint256[] calldata,
        uint256[] calldata,
        bytes calldata
    ) external pure returns (bytes4) {
        return IERC1155Receiver.onERC1155BatchReceived.selector;
    }

    /// @notice ERC-165: the ERC-1155 receiver interface, which that standard
    ///         requires receivers to declare.
    /// @param interfaceId the interface asked about
    /// @return whether this delegate implements it
    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IERC1155Receiver).interfaceId
            || interfaceId == type(IERC165).interfaceId;
    }
}
