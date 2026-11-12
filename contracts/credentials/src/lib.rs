//! Pactora Work Credentials: a soulbound NEP-171 token (with NEP-177
//! metadata and NEP-181 enumeration, via near-contract-standards), one per
//! settled Pactora job. The token id is the Pactora chain job id; the metadata
//! says who paid the worker, how much, and the spec, result and review hashes.
//!
//! Only the minter account mints, and it pays the storage of each token
//! (the excess deposit is refunded). Tokens cannot be transferred: they record
//! work done by their owner, so `nft_transfer` and `nft_transfer_call` panic.
//! There is no approval extension and no burn.

use near_contract_standards::non_fungible_token::core::NonFungibleTokenCore;
use near_contract_standards::non_fungible_token::metadata::{
    NFTContractMetadata, NonFungibleTokenMetadataProvider, TokenMetadata, NFT_METADATA_SPEC,
};
use near_contract_standards::non_fungible_token::{
    NonFungibleToken, NonFungibleTokenEnumeration, NonFungibleTokenResolver, Token, TokenId,
};
use near_sdk::borsh::BorshSerialize;
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, PanicOnDefault, PromiseOrValue};
use std::collections::HashMap;

/// NEP-177 metadata spec this contract reports: the standard library's own
/// constant, so wallets and indexers read it as the standard intends.
pub const SPEC: &str = NFT_METADATA_SPEC;
pub const NAME: &str = "Pactora Work Credentials";
pub const SYMBOL: &str = "PWORK";
const SOULBOUND: &str = "Soulbound: work credentials cannot be transferred";

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    OwnerById,
    TokenMetadata,
    Enumeration,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    minter: AccountId,
    tokens: NonFungibleToken,
}

#[near]
impl Contract {
    /// Only the contract account itself may initialise (deploy and `new` in
    /// one batch), so nobody can claim the roles on fresh code.
    #[init]
    #[private]
    pub fn new(owner: AccountId, minter: AccountId) -> Self {
        let tokens = NonFungibleToken::new(
            StorageKey::OwnerById,
            owner.clone(),
            Some(StorageKey::TokenMetadata),
            Some(StorageKey::Enumeration),
            None::<StorageKey>,
        );
        Self {
            owner,
            minter,
            tokens,
        }
    }

    /// Minter only. The attached deposit must cover the token's storage; the
    /// rest is refunded to the minter. A token id that exists already panics.
    /// Emits the standard NEP-171 `nft_mint` event.
    #[payable]
    pub fn nft_mint(
        &mut self,
        token_id: TokenId,
        receiver_id: AccountId,
        metadata: TokenMetadata,
    ) -> Token {
        require!(env::predecessor_account_id() == self.minter, "NotMinter");
        self.tokens
            .internal_mint(token_id, receiver_id, Some(metadata))
    }

    /// Owner only, with exactly 1 yoctoNEAR (a full-access key): hand the mint
    /// role to another account.
    #[payable]
    pub fn set_minter(&mut self, minter: AccountId) {
        near_sdk::assert_one_yocto();
        require!(env::predecessor_account_id() == self.owner, "NotOwner");
        self.minter = minter;
    }

    pub fn minter(&self) -> AccountId {
        self.minter.clone()
    }

    pub fn owner(&self) -> AccountId {
        self.owner.clone()
    }
}

#[near]
impl NonFungibleTokenCore for Contract {
    #[payable]
    #[allow(unused_variables)]
    fn nft_transfer(
        &mut self,
        receiver_id: AccountId,
        token_id: TokenId,
        approval_id: Option<u64>,
        memo: Option<String>,
    ) {
        env::panic_str(SOULBOUND)
    }

    #[payable]
    #[allow(unused_variables)]
    fn nft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        token_id: TokenId,
        approval_id: Option<u64>,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<bool> {
        env::panic_str(SOULBOUND)
    }

    fn nft_token(&self, token_id: TokenId) -> Option<Token> {
        self.tokens.nft_token(token_id)
    }
}

/// Part of NEP-171. Nothing can be transferred, so nothing ever calls it; it
/// delegates to the standard so the interface is complete.
#[near]
impl NonFungibleTokenResolver for Contract {
    #[private]
    fn nft_resolve_transfer(
        &mut self,
        previous_owner_id: AccountId,
        receiver_id: AccountId,
        token_id: TokenId,
        approved_account_ids: Option<HashMap<AccountId, u64>>,
    ) -> bool {
        self.tokens.nft_resolve_transfer(
            previous_owner_id,
            receiver_id,
            token_id,
            approved_account_ids,
        )
    }
}

#[near]
impl NonFungibleTokenEnumeration for Contract {
    fn nft_total_supply(&self) -> U128 {
        self.tokens.nft_total_supply()
    }

    fn nft_tokens(&self, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.tokens.nft_tokens(from_index, limit)
    }

    fn nft_supply_for_owner(&self, account_id: AccountId) -> U128 {
        self.tokens.nft_supply_for_owner(account_id)
    }

    fn nft_tokens_for_owner(
        &self,
        account_id: AccountId,
        from_index: Option<U128>,
        limit: Option<u64>,
    ) -> Vec<Token> {
        self.tokens
            .nft_tokens_for_owner(account_id, from_index, limit)
    }
}

#[near]
impl NonFungibleTokenMetadataProvider for Contract {
    fn nft_metadata(&self) -> NFTContractMetadata {
        NFTContractMetadata {
            spec: SPEC.to_string(),
            name: NAME.to_string(),
            symbol: SYMBOL.to_string(),
            icon: None,
            base_uri: None,
            reference: None,
            reference_hash: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::mock::MockAction;
    use near_sdk::test_utils::{get_created_receipts, VMContextBuilder};
    use near_sdk::{testing_env, NearToken};

    const CONTRACT: &str = "credentials.test";
    const OWNER: &str = "owner.test";
    const MINTER: &str = "minter.test";
    const WORKER: &str = "worker.test";
    /// Far more than one credential's storage (a few hundred bytes).
    const ONE_NEAR: u128 = 10u128.pow(24);

    fn id(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn ctx(pred: &str, deposit: u128) {
        let mut b = VMContextBuilder::new();
        b.current_account_id(id(CONTRACT))
            .predecessor_account_id(id(pred))
            .signer_account_id(id(pred))
            .attached_deposit(NearToken::from_yoctonear(deposit));
        testing_env!(b.build());
    }

    fn setup() -> Contract {
        ctx(CONTRACT, 0);
        Contract::new(id(OWNER), id(MINTER))
    }

    fn meta(job: u64) -> TokenMetadata {
        TokenMetadata {
            title: Some(format!("Pactora job #{job}")),
            description: Some("Work credential".into()),
            issued_at: Some("1731400000000".into()),
            extra: Some(format!("{{\"chainJobId\":\"{job}\"}}")),
            media: Some("data:image/svg+xml;utf8,<svg/>".into()),
            ..Default::default()
        }
    }

    fn mint(c: &mut Contract, job: u64, to: &str) -> Token {
        ctx(MINTER, ONE_NEAR);
        c.nft_mint(job.to_string(), id(to), meta(job))
    }

    fn panics<F: FnOnce()>(f: F) -> String {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match r {
            Ok(()) => panic!("expected a panic, the call succeeded"),
            Err(e) => e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default(),
        }
    }

    #[test]
    fn roles_read_back_and_only_owner_sets_minter() {
        let mut c = setup();
        assert_eq!(c.owner(), id(OWNER));
        assert_eq!(c.minter(), id(MINTER));
        ctx(MINTER, 1);
        let msg = panics(|| c.set_minter(id("mallory.test")));
        assert!(msg.contains("NotOwner"), "{msg}");
        assert_eq!(c.minter(), id(MINTER));
        // Exactly 1 yoctoNEAR: a function-call key (which cannot attach a
        // deposit) must never be able to rotate the minter.
        ctx(OWNER, 0);
        let msg = panics(|| c.set_minter(id("minter2.test")));
        assert!(msg.contains("1 yoctoNEAR"), "{msg}");
        assert_eq!(c.minter(), id(MINTER));
        ctx(OWNER, 1);
        c.set_minter(id("minter2.test"));
        assert_eq!(c.minter(), id("minter2.test"));
    }

    #[test]
    fn only_the_minter_mints() {
        let mut c = setup();
        for who in [OWNER, WORKER, "mallory.test"] {
            ctx(who, ONE_NEAR);
            let msg = panics(|| {
                c.nft_mint("1".into(), id(WORKER), meta(1));
            });
            assert!(msg.contains("NotMinter"), "{who}: {msg}");
        }
        assert_eq!(c.nft_total_supply().0, 0);
        let t = mint(&mut c, 1, WORKER);
        assert_eq!(t.owner_id, id(WORKER));
        assert_eq!(c.nft_token("1".into()).unwrap().owner_id, id(WORKER));
    }

    #[test]
    fn a_duplicate_token_id_is_refused() {
        let mut c = setup();
        mint(&mut c, 7, WORKER);
        ctx(MINTER, ONE_NEAR);
        let msg = panics(|| {
            c.nft_mint("7".into(), id("other.test"), meta(7));
        });
        assert!(msg.contains("token_id must be unique"), "{msg}");
        assert_eq!(c.nft_token("7".into()).unwrap().owner_id, id(WORKER));
        assert_eq!(c.nft_total_supply().0, 1);
    }

    #[test]
    fn transfer_is_refused() {
        let mut c = setup();
        mint(&mut c, 3, WORKER);
        ctx(WORKER, 1);
        let msg = panics(|| c.nft_transfer(id("buyer.test"), "3".into(), None, None));
        assert!(
            msg.contains("Soulbound: work credentials cannot be transferred"),
            "{msg}"
        );
        assert_eq!(c.nft_token("3".into()).unwrap().owner_id, id(WORKER));
    }

    #[test]
    fn transfer_call_is_refused() {
        let mut c = setup();
        mint(&mut c, 4, WORKER);
        ctx(WORKER, 1);
        let msg = panics(|| {
            let _ = c.nft_transfer_call(id("market.test"), "4".into(), None, None, "sell".into());
        });
        assert!(
            msg.contains("Soulbound: work credentials cannot be transferred"),
            "{msg}"
        );
        assert_eq!(c.nft_token("4".into()).unwrap().owner_id, id(WORKER));
    }

    #[test]
    fn enumeration_and_metadata_read_back() {
        let mut c = setup();
        mint(&mut c, 1, WORKER);
        mint(&mut c, 2, WORKER);
        mint(&mut c, 9, "other.test");
        assert_eq!(c.nft_total_supply().0, 3);
        assert_eq!(c.nft_supply_for_owner(id(WORKER)).0, 2);
        assert_eq!(c.nft_supply_for_owner(id("nobody.test")).0, 0);
        let mut ids: Vec<String> = c
            .nft_tokens_for_owner(id(WORKER), None, None)
            .into_iter()
            .map(|t| t.token_id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(c.nft_tokens(None, None).len(), 3);
        let t = c.nft_token("2".into()).unwrap();
        assert_eq!(t.metadata, Some(meta(2)));
        assert_eq!(t.metadata.unwrap().title.as_deref(), Some("Pactora job #2"));

        let m = c.nft_metadata();
        assert_eq!(m.spec, NFT_METADATA_SPEC);
        assert_eq!(m.name, "Pactora Work Credentials");
        assert_eq!(m.symbol, "PWORK");
    }

    #[test]
    fn the_storage_deposit_is_enforced() {
        let mut c = setup();
        ctx(MINTER, 1);
        let msg = panics(|| {
            c.nft_mint("5".into(), id(WORKER), meta(5));
        });
        assert!(msg.contains("to cover storage"), "{msg}");
        // No state check here: the unit-test mock keeps writes made before a
        // panic, while on chain the whole receipt (the token too) reverts.
    }

    #[test]
    fn excess_deposit_is_refunded_to_the_minter() {
        let mut c = setup();
        mint(&mut c, 6, WORKER);
        let refunds: Vec<u128> = get_created_receipts()
            .into_iter()
            .filter(|r| r.receiver_id == id(MINTER))
            .flat_map(|r| r.actions)
            .filter_map(|a| match a {
                MockAction::Transfer { deposit, .. } => Some(deposit.as_yoctonear()),
                _ => None,
            })
            .collect();
        assert_eq!(refunds.len(), 1, "one refund to the minter");
        assert!(refunds[0] > 0 && refunds[0] < ONE_NEAR);
    }
}
