// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ERC20} from "openzeppelin-contracts/token/ERC20/ERC20.sol";

/// @title MockUSDC — 6-decimal test token
/// @notice Deployed only where `networks.json` sets
///         `paymentToken.deployWithProtocol = true` (local + Monad testnet).
///         On mainnet the protocol points at the canonical USDC instead, with
///         no code change — see docs/08 §2.
/// @dev Public `mint` on purpose: demos must not be faucet-limited. This
///      contract must never be deployed to a production network, and the
///      deploy script refuses to do so.
contract MockUSDC is ERC20 {
    uint8 private constant DECIMALS = 6;

    constructor() ERC20("Mock USD Coin", "mUSDC") {}

    function decimals() public pure override returns (uint8) {
        return DECIMALS;
    }

    /// @notice Anyone may mint. Test token only.
    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}
