// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {Deploy} from "../../script/Deploy.s.sol";
import {Test} from "forge-std/Test.sol";

contract DeployHarness is Deploy {
    function checkRoleHolder(string memory name, address holder, address fallback_, bool isTestnet)
        external
        view
        returns (address)
    {
        return _checkRoleHolder(name, holder, fallback_, isTestnet);
    }
}

/// @dev v3: off testnet, every role holder (arbiter, fee and slash
///      recipients, pauser) must be named and must be a contract — a Safe.
///      On testnet they default to the deployer; on mainnet that default
///      would hand real fees or a dispute vote to a hot key because one
///      variable was forgotten.
contract DeployGuardsTest is Test {
    DeployHarness internal d;
    address internal safe;
    address internal deployer = makeAddr("deployer");

    function setUp() public {
        d = new DeployHarness();
        safe = address(new SafeStandIn());
    }

    function test_testnetDefaultsAnUnsetHolderToTheFallback() public view {
        assertEq(d.checkRoleHolder("ARBITER_ADDRESS", address(0), deployer, true), deployer);
    }

    function test_testnetKeepsANamedHolder() public view {
        assertEq(d.checkRoleHolder("ARBITER_ADDRESS", safe, deployer, true), safe);
    }

    function test_offTestnetRefusesAnUnsetHolder() public {
        vm.expectRevert(bytes("ARBITER_ADDRESS: must be set to a Safe off testnet"));
        d.checkRoleHolder("ARBITER_ADDRESS", address(0), deployer, false);
    }

    function test_offTestnetRefusesAHolderWithoutCode() public {
        vm.expectRevert(bytes("PAUSER_ADDRESS: must be a contract (a Safe) off testnet"));
        d.checkRoleHolder("PAUSER_ADDRESS", makeAddr("an EOA"), deployer, false);
    }

    function test_offTestnetKeepsASafe() public view {
        assertEq(d.checkRoleHolder("FEE_RECIPIENT", safe, deployer, false), safe);
    }
}

contract SafeStandIn {}
