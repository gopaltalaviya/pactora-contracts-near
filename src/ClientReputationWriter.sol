// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {IReputationRegistry} from "./interfaces/IERC8004.sol";

/// @title ClientReputationWriter — the address AGENTX rates CLIENTS from
/// @notice ERC-8004 keys feedback by the address that wrote it, and a reader
///         names the addresses it trusts. Worker reviews come from the escrow;
///         client reviews come from here. A reader that filters on one never
///         sees the other, even if it passes empty tags.
///
///         Only the escrow may write, and only after a settlement or a ruling:
///         a client earns a review by paying (100), by a dispute the arbiter
///         upheld (100), by staying silent until the work auto-approved (50),
///         or loses one by a dispute the arbiter rejected (0).
contract ClientReputationWriter {
    /// @notice The only caller.
    address public immutable escrow;
    /// @notice Where reviews are written.
    IReputationRegistry public immutable reputationRegistry;

    error NotEscrow();
    error ZeroAddress();

    /// @param escrow_ the TaskEscrow that will call `rate`
    /// @param reputationRegistry_ the escrow's ERC-8004 Reputation Registry
    constructor(address escrow_, IReputationRegistry reputationRegistry_) {
        if (escrow_ == address(0) || address(reputationRegistry_) == address(0)) {
            revert ZeroAddress();
        }
        escrow = escrow_;
        reputationRegistry = reputationRegistry_;
    }

    /// @notice Write one review of a client. Escrow only. Reverts if the
    ///         registry does; the escrow treats that as best-effort.
    /// @param clientAgentId the client's ERC-8004 id
    /// @param value 0-100
    /// @param feedbackHash binds the review to the job (same scheme as the escrow's)
    function rate(uint256 clientAgentId, int128 value, bytes32 feedbackHash) external {
        if (msg.sender != escrow) revert NotEscrow();
        reputationRegistry.giveFeedback(
            clientAgentId, value, 0, "agentx", "client", "", "", feedbackHash
        );
    }
}
