// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccountCore} from "./AgentAccountCore.sol";
import {IERC1271} from "openzeppelin-contracts/interfaces/IERC1271.sol";
import {Initializable} from "openzeppelin-contracts/proxy/utils/Initializable.sol";
import {SignatureChecker} from "openzeppelin-contracts/utils/cryptography/SignatureChecker.sol";

/// @title AgentAccount — an agent's wallet, with spending limits enforced
///        ON-CHAIN rather than on someone's server
///
/// @notice A clone the factory creates: the owner (a human, or a multisig)
///         moves funds in and grants the agent's hot key a session. Every
///         spend control is in `AgentAccountCore`; this adds the owner, its
///         two-step transfer, and ERC-1271 for the owner's signature.
///
///         For an EOA that should keep its funds where they are, the same
///         controls come by EIP-7702 delegation instead: `AgentDelegate`.
contract AgentAccount is AgentAccountCore, Initializable {
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    constructor() {
        // The implementation behind the clones must never be initialisable.
        _disableInitializers();
    }

    /// @notice Called once, by the factory, on a fresh clone.
    /// @param owner_ the human (or multisig) that controls the account
    /// @param token_ the token whose outflow the caps measure
    /// @param policy_ the caps
    function initialize(address owner_, address token_, Policy calldata policy_)
        external
        initializer
    {
        if (owner_ == address(0)) revert ZeroAddress();
        _layout().owner = owner_;
        _init(owner_, token_, policy_);
    }

    /// @inheritdoc AgentAccountCore
    function owner() public view override returns (address) {
        return _layout().owner;
    }

    /// @notice The owner named by `transferOwnership`, until they accept.
    function pendingOwner() external view returns (address) {
        return _layout().pendingOwner;
    }

    function _domainName() internal pure override returns (string memory) {
        return "AgentAccount";
    }

    /// @notice Begin handing the account to `newOwner`, who must accept.
    ///         Two steps, so a typo cannot hand an account to nobody.
    /// @param newOwner the proposed owner
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        _layout().pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner(), newOwner);
    }

    /// @notice Accept a pending transfer. The pending owner only.
    function acceptOwnership() external {
        Layout storage s = _layout();
        if (msg.sender != s.pendingOwner) revert NotOwner();
        emit OwnershipTransferred(s.owner, msg.sender);
        s.owner = msg.sender;
        s.pendingOwner = address(0);
        // Audit F4: the old owner's hot keys and renewer go with it. Standing
        // token allowances cannot be enumerated here: the old owner's
        // `setAllowance` grants survive, so reset them before transferring.
        _retireSessionKeys();
    }

    /// @notice ERC-1271. Valid only when the OWNER signed `accountMessageHash`.
    ///         The canonical ERC-8004 registry needs this to accept the account
    ///         as an agent's wallet. A session key's signature never counts: a
    ///         stolen hot key would otherwise sign approvals for anything that
    ///         accepts ERC-1271, outside every cap. An owner that is itself a
    ///         contract (a Safe) signs through its own ERC-1271.
    /// @param hash the digest being checked
    /// @param signature the owner's signature
    /// @return magic `0x1626ba7e` if valid, `0xffffffff` otherwise; never reverts
    function isValidSignature(bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4 magic)
    {
        if (SignatureChecker.isValidSignatureNow(owner(), accountMessageHash(hash), signature)) {
            return IERC1271.isValidSignature.selector;
        }
        return 0xffffffff;
    }
}
