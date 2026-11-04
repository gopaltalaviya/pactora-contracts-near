// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccount} from "./AgentAccount.sol";
import {AgentAccountCore} from "./AgentAccountCore.sol";
import {Clones} from "openzeppelin-contracts/proxy/Clones.sol";

/// @title AgentAccountFactory — deterministic minimal-proxy accounts
/// @dev ERC-1167 clones: an agent wallet costs ~45 bytes of deployed code
///      instead of a full contract. At the volume AGENTX is designed for —
///      thousands of agents — that difference is the difference between
///      onboarding being free and being a fundraise.
///
///      Addresses are CREATE2-deterministic from (owner, salt), so an agent's
///      wallet address can be computed and funded BEFORE it is deployed. Only
///      the owner may create it: otherwise anyone could occupy an owner's
///      predicted address first, with a policy of their choosing, and make the
///      owner's own call revert.
contract AgentAccountFactory {
    address public immutable implementation;
    address public immutable token;

    /// @notice A new account exists at `account`, owned by `owner`.
    event AccountCreated(address indexed account, address indexed owner, bytes32 salt);

    error NotOwner();
    error ZeroAddress();

    /// @param token_ the token every account's caps measure
    constructor(address token_) {
        if (token_ == address(0)) revert ZeroAddress();
        token = token_;
        implementation = address(new AgentAccount());
    }

    /// @notice Create the caller's account.
    /// @param owner must be the caller
    /// @param salt any value; with `owner` it fixes the address
    /// @param policy the initial caps
    /// @return account the new account
    function createAccount(address owner, bytes32 salt, AgentAccountCore.Policy calldata policy)
        external
        returns (address account)
    {
        if (msg.sender != owner) revert NotOwner();
        account = Clones.cloneDeterministic(implementation, _saltOf(owner, salt));
        AgentAccount(payable(account)).initialize(owner, token, policy);
        emit AccountCreated(account, owner, salt);
    }

    /// @notice Where an account WILL live, before it exists.
    /// @param owner the account's owner
    /// @param salt the owner's salt
    /// @return the address
    function predictAddress(address owner, bytes32 salt) external view returns (address) {
        return
            Clones.predictDeterministicAddress(implementation, _saltOf(owner, salt), address(this));
    }

    /// @dev Binding the owner into the salt stops one owner from front-running
    ///      another's chosen salt to occupy their address.
    function _saltOf(address owner, bytes32 salt) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(owner, salt));
    }
}
