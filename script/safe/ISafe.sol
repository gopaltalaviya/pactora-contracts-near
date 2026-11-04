// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// @dev The slice of Safe v1.4.1 the deploy script and fork test use. The
///      deployments are Safe's canonical addresses, verified on Monad testnet
///      10143 on 2026-10-25 (code present, VERSION() == "1.4.1"):
///        SafeL2              0x29fcB43b46531BcA003ddC8FCB67FFE91900C762
///        SafeProxyFactory    0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67
///        CompatibilityFallbackHandler 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99
interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function execTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address payable refundReceiver,
        bytes memory signatures
    ) external payable returns (bool success);

    function getTransactionHash(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address refundReceiver,
        uint256 nonce
    ) external view returns (bytes32);

    function nonce() external view returns (uint256);
    function getOwners() external view returns (address[] memory);
    function getThreshold() external view returns (uint256);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
}

library SafeAddresses {
    address internal constant SAFE_L2 = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant PROXY_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant FALLBACK_HANDLER = 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99;
}
