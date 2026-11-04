// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {Test} from "forge-std/Test.sol";

contract MockUSDCTest is Test {
    MockUSDC internal token;
    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    function setUp() public {
        token = new MockUSDC();
    }

    /// The whole money model assumes 6 decimals. If this ever changes, every
    /// amount in the protocol is wrong by a factor of 10^12.
    function test_decimalsAreSix() public view {
        assertEq(token.decimals(), 6, "payment token must be 6dp");
    }

    function test_oneCentIsTenThousandBaseUnits() public view {
        assertEq(10 ** token.decimals() / 100, 10_000);
    }

    /// 0.02 USDC - the demo's research-agent price - must round-trip exactly.
    function test_demoPriceIsExact() public {
        uint256 twoCents = 20_000;
        token.mint(alice, twoCents);
        assertEq(token.balanceOf(alice), twoCents);
    }

    function test_anyoneCanMint() public {
        vm.prank(alice);
        token.mint(bob, 1_000_000);
        assertEq(token.balanceOf(bob), 1_000_000);
    }

    function testFuzz_mintThenTransferPreservesSupply(uint128 amount, uint128 send) public {
        send = uint128(bound(send, 0, amount));
        token.mint(alice, amount);

        vm.prank(alice);
        token.transfer(bob, send);

        assertEq(token.balanceOf(alice) + token.balanceOf(bob), amount);
        assertEq(token.totalSupply(), amount);
    }
}
