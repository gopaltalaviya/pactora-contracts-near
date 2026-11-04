// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {AgentAccount} from "../../src/AgentAccount.sol";
import {AgentAccountCore} from "../../src/AgentAccountCore.sol";
import {AgentDelegate} from "../../src/AgentDelegate.sol";
import {AgentAccountTest} from "./AgentAccount.t.sol";
import {ERC1155} from "openzeppelin-contracts/token/ERC1155/ERC1155.sol";
import {ERC721} from "openzeppelin-contracts/token/ERC721/ERC721.sol";

/// @dev Mints the way the canonical ERC-8004 identity registry does: with
///      `_safeMint`, which calls `onERC721Received` on any receiver with code.
contract SafeMinter is ERC721("Probe", "P") {
    uint256 internal next = 1;

    function mint() external returns (uint256 id) {
        id = next++;
        _safeMint(msg.sender, id);
    }
}

contract Probe1155 is ERC1155("") {
    function mint(address to, uint256 id, uint256 amount) external {
        _mint(to, id, amount, "");
    }

    function mintBatch(address to, uint256[] calldata ids, uint256[] calldata amounts) external {
        _mintBatch(to, ids, amounts, "");
    }
}

/// @dev Exposes the namespace constant, to check it against ERC-7201's formula.
contract LayoutProbe is AgentDelegate {
    function slot() external pure returns (bytes32) {
        return LAYOUT_SLOT;
    }
}

/// @dev v3: EVERY AgentAccount test, run again against an EOA delegated to
///      AgentDelegate — the delegate's code placed at the EOA's address with
///      `vm.etch`, which is what EIP-7702 does for execution: the code runs
///      with the EOA's address and the EOA's storage, and the owner is the EOA
///      calling itself. The pinned EVM (cancun) has no type-4 transactions; the
///      delegation itself was proven live on Monad testnet
///      (agentx-backend scripts/spike-7702.mjs, verify-delegate.mjs).
///
///      The six AgentAccount tests about what a clone has and a delegate does
///      not (a factory, an owner field to transfer, a contract owner) are
///      replaced below by the delegate's own versions.
contract AgentDelegateTest is AgentAccountTest {
    uint256 internal eoaKey;
    AgentDelegate internal delegateCode;

    function _delegate(address eoa) internal {
        if (address(delegateCode) == address(0)) delegateCode = new AgentDelegate();
        vm.etch(eoa, address(delegateCode).code);
    }

    function _makeAccount() internal override returns (AgentAccount a) {
        (agentOwner, eoaKey) = makeAddrAndKey("delegating-eoa");
        _delegate(agentOwner);
        vm.prank(agentOwner); // the EOA, calling itself
        AgentDelegate(payable(agentOwner))
            .initialize(
                address(token),
                AgentAccountCore.Policy({
                    perTaskCap: PER_TASK_CAP, dailyCap: DAILY_CAP, allowlistOnly: true
                })
            );
        // Same selectors for everything the two share, so the inherited tests
        // drive the delegate through the AgentAccount type.
        a = AgentAccount(payable(agentOwner));
        assertEq(a.owner(), agentOwner, "the EOA owns itself");
    }

    // ─────────────── the delegate's own versions of six tests ───────────────

    function test_aNewOwnerInheritsNoSessionKeysOrManager() public override {
        // Nothing to inherit: a delegated EOA's owner is the key itself and
        // cannot be transferred (test_ownershipTransferTakesTwoSteps).
    }

    function test_ownershipTransferTakesTwoSteps() public override {
        // There is nothing to transfer: the key IS the owner.
        vm.prank(agentOwner);
        (bool ok,) = address(account)
            .call(abi.encodeCall(AgentAccount.transferOwnership, (makeAddr("other"))));
        assertFalse(ok, "a delegate has no transferOwnership");
        assertEq(account.owner(), agentOwner);
    }

    function test_implementationCannotBeInitialized() public override {
        // A freshly delegated EOA: nobody but the EOA may initialise it.
        (address fresh,) = makeAddrAndKey("fresh-eoa");
        _delegate(fresh);
        AgentAccountCore.Policy memory p =
            AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true});
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        AgentDelegate(payable(fresh)).initialize(address(token), p);

        vm.prank(fresh);
        AgentDelegate(payable(fresh)).initialize(address(token), p);
        vm.prank(fresh);
        vm.expectRevert(AgentDelegate.AlreadyInitialized.selector);
        AgentDelegate(payable(fresh)).initialize(address(token), p);

        // And the deployed contract itself can only be initialised by itself.
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        delegateCode.initialize(address(token), p);
    }

    function test_theOwnersSignatureIsValidForTheAccount() public view override {
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(eoaKey, account.accountMessageHash(h));
        assertEq(account.isValidSignature(h, abi.encodePacked(r, s_, v)), MAGIC);
    }

    function test_aSignatureOverTheBareHashIsRefused() public view override {
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(eoaKey, h);
        assertTrue(account.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC);
    }

    function test_oneAccountsSignatureIsNotValidForAnother() public override {
        // The same key owns an AgentAccount clone too; a signature for the
        // delegate's domain must not be valid for the clone, nor the reverse.
        vm.prank(agentOwner);
        AgentAccount clone = AgentAccount(
            payable(factory.createAccount(
                    agentOwner,
                    bytes32("clone"),
                    AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true})
                ))
        );
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(eoaKey, account.accountMessageHash(h));
        assertEq(account.isValidSignature(h, abi.encodePacked(r, s_, v)), MAGIC);
        assertTrue(
            clone.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC, "not the clone's"
        );
        (v, r, s_) = vm.sign(eoaKey, clone.accountMessageHash(h));
        assertEq(clone.isValidSignature(h, abi.encodePacked(r, s_, v)), MAGIC);
        assertTrue(
            account.isValidSignature(h, abi.encodePacked(r, s_, v)) != MAGIC, "not the delegate's"
        );
    }

    function test_aContractOwnerSignsThroughItsOwnErc1271() public override {
        // The owner is always the EOA's key, so the case does not arise. What
        // must hold instead: a wrong signature is refused CHEAPLY. Asking
        // SignatureChecker would call isValidSignature on this very address
        // (it has code now) and recurse until the gas ran out.
        (, uint256 other) = makeAddrAndKey("not-the-owner");
        bytes32 h = keccak256("registry digest");
        (uint8 v, bytes32 r, bytes32 s_) = vm.sign(other, account.accountMessageHash(h));
        uint256 before = gasleft();
        bytes4 got = account.isValidSignature(h, abi.encodePacked(r, s_, v));
        uint256 used = before - gasleft();
        assertTrue(got != MAGIC);
        assertLt(used, 30_000, "no recursion");
    }

    function test_ownerCanSweep() public override {
        // The account and its owner are one address here, so a sweep to the
        // owner moves nothing; the kill switch is a sweep to somewhere else.
        address cold = makeAddr("cold-wallet");
        uint256 bal = token.balanceOf(address(account));
        vm.prank(agentOwner);
        account.sweep(cold, bal);
        assertEq(token.balanceOf(cold), bal);
        assertEq(token.balanceOf(address(account)), 0);
    }

    // ─────────────── what only a delegate has to get right ───────────────

    function test_theNamespaceIsErc7201() public {
        bytes32 expected = keccak256(
            abi.encode(uint256(keccak256("agentx.storage.AgentAccount")) - 1)
        ) & ~bytes32(uint256(0xff));
        assertEq(new LayoutProbe().slot(), expected);
    }

    function test_whatAnEarlierDelegateLeftIsNeverRead() public {
        // An EOA that used another delegate before: its slots 0..9 hold that
        // delegate's state — here, addresses that would read as an owner, a
        // pending owner and a token in a sequential layout.
        (address used,) = makeAddrAndKey("used-eoa");
        for (uint256 i; i < 10; i++) {
            vm.store(used, bytes32(i), bytes32(uint256(uint160(attacker))));
        }
        _delegate(used);
        vm.prank(used);
        AgentDelegate(payable(used))
            .initialize(
                address(token),
                AgentAccountCore.Policy({perTaskCap: 7, dailyCap: 8, allowlistOnly: true})
            );
        AgentDelegate d = AgentDelegate(payable(used));
        assertEq(d.owner(), used);
        assertEq(address(d.token()), address(token));
        (uint128 perTask, uint128 daily, bool only) = d.policy();
        assertEq(perTask, 7);
        assertEq(daily, 8);
        assertTrue(only);
        vm.prank(attacker);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        d.sweep(attacker, 1);
        assertEq(vm.load(used, bytes32(0)), bytes32(uint256(uint160(attacker))), "left alone");
    }

    function test_aZeroTokenIsRefused() public {
        (address fresh,) = makeAddrAndKey("zero-token-eoa");
        _delegate(fresh);
        vm.prank(fresh);
        vm.expectRevert(AgentAccountCore.ZeroAddress.selector);
        AgentDelegate(payable(fresh))
            .initialize(
                address(0),
                AgentAccountCore.Policy({perTaskCap: 1, dailyCap: 1, allowlistOnly: true})
            );
    }

    /// @dev Found live: the canonical identity registry mints with
    ///      `_safeMint`, and a delegated EOA HAS CODE, so the mint asked it for
    ///      `onERC721Received` — which it lacked — and the EOA could not even
    ///      register its own ERC-8004 identity (ERC721InvalidReceiver). A plain
    ///      EOA accepts any safe transfer; delegating must not take that away.
    function test_aDelegatedEoaStillAcceptsSafeTransfers() public {
        SafeMinter nft = new SafeMinter();
        vm.prank(address(account));
        uint256 id = nft.mint();
        assertEq(nft.ownerOf(id), address(account), "ERC-721 safe mint");

        Probe1155 multi = new Probe1155();
        multi.mint(address(account), 7, 3);
        assertEq(multi.balanceOf(address(account), 7), 3, "ERC-1155 mint");
        uint256[] memory ids = new uint256[](2);
        uint256[] memory amounts = new uint256[](2);
        (ids[0], ids[1], amounts[0], amounts[1]) = (1, 2, 4, 5);
        multi.mintBatch(address(account), ids, amounts);
        assertEq(multi.balanceOf(address(account), 2), 5, "ERC-1155 batch mint");
    }

    function test_itDeclaresTheErc1155ReceiverInterface() public view {
        AgentDelegate d = AgentDelegate(payable(address(account)));
        assertTrue(d.supportsInterface(0x4e2312e0), "IERC1155Receiver");
        assertTrue(d.supportsInterface(0x01ffc9a7), "IERC165");
        assertFalse(d.supportsInterface(0xffffffff), "never the invalid id");
        assertFalse(d.supportsInterface(0x80ac58cd), "not an ERC-721 token");
    }

    function test_theOwnerIsTheEoaCallingItself() public {
        // Not the EOA's address as an outside caller in some other context:
        // only a call FROM the EOA counts, and the EOA is the only owner.
        vm.prank(sessionKey);
        vm.expectRevert(AgentAccountCore.NotOwner.selector);
        account.sweep(sessionKey, 1);
        vm.prank(agentOwner);
        account.sweep(agentOwner, 1);
    }
}
