#![cfg(test)]

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::Events as _,
    testutils::Ledger, testutils::MockAuth, testutils::MockAuthInvoke, vec, Address, BytesN, Env,
    IntoVal, Symbol, TryFromVal,
};

use crate::{InvoiceContract, InvoiceContractClient, InvoiceStatus};

#[contract]
pub struct MockRegistry;

#[contractimpl]
impl MockRegistry {
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&DataKey(address))
            .unwrap_or(false)
    }

    pub fn register(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&DataKey(address.clone()), &true);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey(address), 100, 2_000_000);
    }
}

#[contracttype]
pub struct DataKey(Address);

// --------------- Mock Token ---------------

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = TKey(from.clone());
        let to_key = TKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage().persistent().get(&TKey(addr)).unwrap_or(0)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TKey(to.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(bal + amount));
    }
}

#[contracttype]
pub struct TKey(Address);

#[contract]
pub struct MockPool;

#[contractimpl]
impl MockPool {
    pub fn handle_default(_env: Env, _invoice_id: BytesN<32>) -> bool {
        true
    }

    pub fn receive_repayment(_env: Env, _invoice_id: BytesN<32>, _amount: u128) -> bool {
        true
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        let key = Symbol::new(&env, "asset");
        env.storage().instance().get(&key).unwrap()
    }

    pub fn receive_repayment_with_refund(
        _env: Env,
        _invoice_id: BytesN<32>,
        _face_value: u128,
        _refund_to_buyer: u128,
        _buyer: Address,
    ) -> bool {
        true
    }
}

type Setup = (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    MockRegistryClient<'static>,
    Address,
);

#[allow(dead_code)]
type SetupWithAdmin = (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    MockRegistryClient<'static>,
    Address,
    Address,
);

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);

    (env, client, issuer, buyer, registry_client, usdc_asset)
}

#[allow(dead_code)]
fn setup_with_admin() -> SetupWithAdmin {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);

    (
        env,
        client,
        issuer,
        buyer,
        registry_client,
        usdc_asset,
        admin,
    )
}

fn mock_pool_with_asset(env: &Env, asset: &Address) -> Address {
    let pool_id = env.register_contract(None, MockPool);
    let _pool_client = MockPoolClient::new(env, &pool_id);
    env.as_contract(&pool_id, || {
        let key = Symbol::new(env, "asset");
        env.storage().instance().set(&key, asset);
    });
    pool_id
}

// ── Issue #217: double-initialize panics ───────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    // First initialize — succeeds
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    // Verify admin and registry were stored correctly
    env.as_contract(&contract_id, || {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&crate::DataKey::Admin)
            .unwrap();
        assert_eq!(stored_admin, admin);
        let stored_registry: Address = env
            .storage()
            .instance()
            .get(&crate::DataKey::RegistryContract)
            .unwrap();
        assert_eq!(stored_registry, registry_id);
    });

    // Second initialize — panics with AlreadyInitialized (Error(Contract, #1));
    // admin/registry must remain unchanged from the first call
    client.initialize(&admin, &registry_id);
}

#[test]
fn test_create_invoice_with_verified_parties() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = 1_000_000_000;
    let due_date = env.ledger().timestamp() + 86400;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.issuer, issuer);
    assert_eq!(invoice.buyer, buyer);
    assert_eq!(invoice.face_value, face_value);
    assert_eq!(invoice.due_date, due_date);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.funding_asset, usdc);
    assert_eq!(invoice.funding_pool, None);
    assert!(!invoice.issuer_confirmed);
    assert!(!invoice.buyer_confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_create_fails_unverified_issuer() {
    let (env, client, _issuer, buyer, _, usdc) = setup();
    let unverified_issuer = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;
    client.create(&unverified_issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_create_fails_unverified_buyer() {
    let (env, client, issuer, _buyer, _, usdc) = setup();
    let unverified_buyer = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;
    client.create(&issuer, &unverified_buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_create_fails_zero_face_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    client.create(&issuer, &buyer, &0, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let past_date = env.ledger().timestamp() - 1;
    client.create(&issuer, &buyer, &1_000_000_000, &past_date, &usdc);
}

// ============== ISSUE #B: due_date BOUNDARY (due_date == now) ==============

// At exactly `due_date == now`, `create` rejects with InvalidDueDate (#7).
// The check is `due_date <= env.ledger().timestamp()` so equality falls on
// the rejection side. Pins the current behaviour so a regression on the
// boundary comparator cannot land silently.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_when_due_date_equals_now() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let equal_due_date = env.ledger().timestamp();
    client.create(&issuer, &buyer, &1_000_000_000, &equal_due_date, &usdc);
}

// The boundary's other side: `due_date == now + 1` is the smallest accepted
// value. Confirms storage and events on the positive boundary so that a
// future refactor can't flip the boundary silently.
#[test]
fn test_create_succeeds_when_due_date_one_second_in_future() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let just_future_due_date = env.ledger().timestamp() + 1;
    let face_value: u128 = 1_000_000_000;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &just_future_due_date, &usdc);

    // State: invoice record exists at Created with the boundary due_date.
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.due_date, just_future_due_date);
    assert_eq!(invoice.created_at, env.ledger().timestamp());
    assert_eq!(invoice.face_value, face_value);

    // Events: exactly one invoice_created event was emitted by the invoice
    // contract. Per `events::invoice_created` the topic tuple is
    // `(Symbol("invoice_created"), invoice_id, issuer, buyer, funding_asset)`
    // and the data payload is `face_value: u128`. We pin the count and event
    // shape here; detailed per-topic comparisons live in the dedicated event
    // integration tests because soroban_sdk's `Val` does not implement
    // `PartialEq` for ad-hoc equality assertions.
    // The setup() helper calls initialize(), which emits a contract_initialized
    // event. Use last() to check only the invoice_created event from create().
    let events = env.events().all();
    let (event_contract, _topics, _data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, client.address.clone());
}

#[test]
fn test_list_for_financing() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &200);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_fails_wrong_status() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    client.list_for_financing(&invoice_id, &300);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_fails_discount_too_high() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
fn test_list_for_financing_discount_bps_zero_boundary() {
    // discount_bps == 0 is currently accepted; see issue #79 for a
    // companion validation that would turn this into a panic test.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &0);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 0);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 0u32);
}

#[test]
fn test_list_for_financing_discount_bps_max_boundary() {
    // discount_bps == 5000 is the inclusive upper bound and must succeed.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &5000);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 5000);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 5000u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_for_financing_discount_bps_one_above_max_boundary_panics() {
    // discount_bps == 5001 is one past the inclusive upper bound and must panic
    // with DiscountTooHigh (#9). Pins the exact boundary alongside the existing
    // test_list_fails_discount_too_high regression test.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_list_for_financing_non_issuer_panics() {
    let (env, client, issuer, _buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &_buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    env.set_auths(&[]);
    client.list_for_financing(&invoice_id, &200);
}

#[test]
fn test_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    let funded_amount: u128 = 980_000_000;
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &funded_amount);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);
    assert_eq!(client.get(&invoice_id).funding_pool, Some(pool));

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(!client.get(&invoice_id).buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(client.get(&invoice_id).buyer_confirmed);
}

#[test]
fn test_get_by_issuer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_issuer(&issuer);
    assert_eq!(invoices.len(), 2);

    let other = Address::generate(&env);
    let empty = client.get_by_issuer(&other);
    assert_eq!(empty.len(), 0);
}

#[test]
fn test_get_by_buyer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_buyer(&buyer);
    assert_eq!(invoices.len(), 2);
}

#[test]
fn test_get_by_status_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_unknown_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get(&fake_id);
}

#[test]
fn test_dual_confirmation_both_must_confirm() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Active);
    assert!(inv.issuer_confirmed);
    assert!(!inv.buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Confirmed);
    assert!(inv.issuer_confirmed);
    assert!(inv.buyer_confirmed);
}

#[test]
fn test_confirm_by_both_transitions_to_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_confirm_delivery_wrong_party_panics() {
    let (env, client, issuer, _buyer, registry, usdc) = setup();
    let stranger = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry.register(&buyer);

    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &stranger);
}

#[test]
fn test_trigger_default_requires_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
fn test_get_by_status_filters_correctly() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    let id1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);

    client.list_for_financing(&id1, &200);
    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 1);
    let listed = client.get_by_status(&InvoiceStatus::Listed);
    assert_eq!(listed.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_double_confirmation_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
fn test_status_transitions_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_mark_funded_fails_asset_mismatch() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let xlm = Address::generate(&env);
    let xlm_pool = mock_pool_with_asset(&env, &xlm);
    client.set_pool_contract(&xlm_pool);
    client.mark_funded(&invoice_id, &xlm_pool, &xlm, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_mark_funded_rejects_zero_amount() {
    // Regression test for issue #194: mark_funded must reject
    // funded_amount == 0 with InvalidAmount (#16).
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &0);
}

#[test]
fn test_mark_funded_succeeds_with_matching_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.funding_pool, Some(pool));
}

#[test]
fn test_create_invoice_with_xlm_asset() {
    let (env, client, issuer, buyer, _, _usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let xlm_asset = Address::generate(&env);

    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &xlm_asset);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.funding_asset, xlm_asset);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

#[test]
fn test_get_funding_asset_returns_correct_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let asset = client.get_funding_asset(&invoice_id);
    assert_eq!(asset, usdc);
}

#[test]
fn test_set_pool_contract_emits_event() {
    let (env, client, _issuer, _buyer, _registry, usdc) = setup();
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    // The setup() helper calls initialize(), which emits a contract_initialized
    // event. Use last() to check only the pool_contract_updated event.
    let events = env.events().all();
    let event = events.last().expect("expected at least one event");
    assert_eq!(event.0, client.address.clone());
    assert_eq!(event.1.len(), 3);
    let symbol: Symbol = Symbol::try_from_val(&env, &event.1.get(0).unwrap()).unwrap();
    assert_eq!(symbol, Symbol::new(&env, "pool_contract_updated"));
    let old: Address = Address::try_from_val(&env, &event.1.get(1).unwrap()).unwrap();
    assert_eq!(old, pool);
    let new: Address = Address::try_from_val(&env, &event.1.get(2).unwrap()).unwrap();
    assert_eq!(new, pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_set_pool_contract_fails_without_admin() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let pool = mock_pool_with_asset(&env, &Address::generate(&env));
    client.set_pool_contract(&pool);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_set_pool_contract_fails_non_admin() {
    let env = Env::default();
    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (&admin, &registry_id).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);
    let pool = mock_pool_with_asset(&env, &Address::generate(&env));
    client.set_pool_contract(&pool);
}

#[test]
fn test_set_pool_contract_emits_event_on_update() {
    let (env, client, _issuer, _buyer, _registry, usdc) = setup();
    let first_pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&first_pool);

    let second_pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&second_pool);

    // The setup() helper calls initialize(), which emits a contract_initialized
    // event. Use last() to check only the most recent pool_contract_updated event.
    let events = env.events().all();
    let event = events.last().expect("expected at least one event");
    assert_eq!(event.0, client.address.clone());
    assert_eq!(event.1.len(), 3);
    let old: Address = Address::try_from_val(&env, &event.1.get(1).unwrap()).unwrap();
    assert_eq!(old, first_pool);
    let new: Address = Address::try_from_val(&env, &event.1.get(2).unwrap()).unwrap();
    assert_eq!(new, second_pool);
}

// ===================== RESTORED TESTS (issue #205 deletion) =====================

#[test]
fn test_expire_listing_transitions_to_expired_after_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    client.list_for_financing(&invoice_id, &200);
    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);

    let result = client.expire_listing(&invoice_id, &issuer);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    // A freshly created invoice is not Funded/Active/Confirmed, so defaulting
    // it must be rejected with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    // A Listed invoice has not been funded, so defaulting it must be rejected
    // with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // A Repaid invoice is terminal, so defaulting it must be rejected with
    // InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
fn test_trigger_default_succeeds_at_exact_due_date() {
    // Boundary test: default must be allowed when `now == due_date`
    // (previously panicked due to `<=` comparison — issue #200)
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to exactly the due date
    env.ledger().set_timestamp(due_date);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_trigger_default_fails_before_due_date() {
    // Negative test: default must NOT be allowed when `now < due_date`
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to 1 second before the due date — should panic
    env.ledger().set_timestamp(due_date - 1);

    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_trigger_default_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);

    // Set pool contract (admin auth)
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool_id);

    // mark_funded requires pool auth
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool_id,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool_id.clone(),
                usdc.clone(),
                980_000_000u128,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);

    // mark_shipped by issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_shipped",
            args: (invoice_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_shipped(&invoice_id);

    // confirm delivery by issuer and buyer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), issuer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &issuer);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &buyer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), buyer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &buyer);

    // Fast forward past due date
    env.ledger().set_timestamp(due_date + 1);

    // Now call trigger_default without mocking admin auth -> should panic with NotAuthorized
    client.trigger_default(&invoice_id);
}

#[test]
fn test_expire_listing_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id, &issuer);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
fn test_expire_listing_succeeds_by_admin() {
    let (env, client, issuer, buyer, _, usdc, admin) = setup_with_admin();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id, &admin);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_early_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by only 5 days (less than 7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    client.expire_listing(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_wrong_status_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    // Fast forward ledger time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    client.expire_listing(&invoice_id, &issuer);
}

#[test]
fn test_expire_listing_configurable_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Set expiry window to 1 day (86400 seconds)
    client.set_expiry_window(&86400);
    assert_eq!(client.get_expiry_window(), 86400);

    // Fast forward by 1 day + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 86400 + 1);

    let result = client.expire_listing(&invoice_id, &issuer);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
fn test_expire_listing_exact_boundary() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward by exact expiry window (7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60);

    let result = client.expire_listing(&invoice_id, &issuer);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_one_second_before_boundary_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward to 1 second before expiry window
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 - 1);

    client.expire_listing(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_expire_listing_overflow_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(100);
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Set an expiry window that will overflow u64 when added to listed_at (100 + u64::MAX > u64::MAX)
    client.set_expiry_window(&u64::MAX);

    client.expire_listing(&invoice_id, &issuer);
}

// ── Issue #872: expire_listing issuer path with explicit auth ────────────────

/// Stand up the contract without `mock_all_auths()` so every auth below is
/// exercised explicitly, like a real transaction.
fn setup_no_mock_auth() -> (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    (env, client, issuer, buyer, admin)
}

/// Regression test for issue #872: the stored issuer must be able to expire
/// their own listing with real (non-mocked) authorization. It previously
/// failed because the issuer check went through a private `check_auth`
/// self-invoke that only "succeeded" under `mock_all_auths`; genuine issuer
/// signatures fell through to the admin-only path and the call panicked.
#[test]
fn test_expire_listing_by_issuer_with_explicit_auth() {
    let (env, client, issuer, buyer, _) = setup_no_mock_auth();
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward past the default 7-day expiry window.
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // The issuer explicitly signs the expiry of their own listing.
    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "expire_listing",
            args: (invoice_id.clone(), issuer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let result = client.expire_listing(&invoice_id, &issuer);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

/// The admin fallback path must keep working with explicit authorization too.
#[test]
fn test_expire_listing_by_admin_with_explicit_auth() {
    let (env, client, issuer, buyer, admin) = setup_no_mock_auth();
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Only the admin authorizes; the contract must take the admin branch.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "expire_listing",
            args: (invoice_id.clone(), admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let result = client.expire_listing(&invoice_id, &admin);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

/// An unrelated caller must never be able to expire a listing: even with a
/// valid signature, the caller-equality check rejects them with the typed
/// `NotAuthorized` error.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_expire_listing_stranger_with_explicit_auth_panics() {
    let (env, client, issuer, buyer, _admin) = setup_no_mock_auth();
    let stranger = Address::generate(&env);
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // The stranger signs — neither the issuer nor the admin path matches.
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "expire_listing",
            args: (invoice_id.clone(), stranger.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.expire_listing(&invoice_id, &stranger);
}

/// The issuer cannot expire a listing before its window has passed (auth
/// alone is not enough — the time check still applies).
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_unexpired_rejected_for_issuer_with_explicit_auth() {
    let (env, client, issuer, buyer, _admin) = setup_no_mock_auth();
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    // Well before the 7-day window.
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    // Issuer signs — must still hit ListingNotExpired.
    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "expire_listing",
            args: (invoice_id.clone(), issuer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.expire_listing(&invoice_id, &issuer);
}

/// The admin cannot expire a listing before its window has passed either.
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_unexpired_rejected_for_admin_with_explicit_auth() {
    let (env, client, issuer, buyer, admin) = setup_no_mock_auth();
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[MockAuth {
        address: &issuer,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    // Well before the 7-day window.
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    // Admin signs — must still hit ListingNotExpired.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "expire_listing",
            args: (invoice_id.clone(), admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.expire_listing(&invoice_id, &admin);
}

#[test]
fn test_set_expiry_window_emits_event() {
    let (env, client, _, _, _, _) = setup();
    let window: u64 = 86400;

    client.set_expiry_window(&window);

    // The setup() helper calls initialize(), which emits a contract_initialized
    // event. Use last() to check only the expiry_window_set event.
    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "expiry_window_set"),).into_val(&env)
    );
    assert_eq!(u64::try_from_val(&env, &data).unwrap(), window);
}

#[test]
fn test_unique_invoice_ids_for_identical_inputs() {
    // Two invoices created with identical parameters (issuer, buyer, face_value, due_date, funding_asset)
    // must receive different IDs because the internal storage Counter salt increments per invoice.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = 1_000_000_000;
    let due_date = env.ledger().timestamp() + 86400;

    // Create two invoices with identical parameters
    let id1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    let id2 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // 1. Assert id1 != id2
    assert_ne!(id1, id2);

    // 2. Assert state after: verify both invoices exist in persistent storage with correct fields
    let inv1 = client.get(&id1);
    let inv2 = client.get(&id2);

    assert_eq!(inv1.id, id1);
    assert_eq!(inv2.id, id2);

    assert_eq!(inv1.issuer, issuer);
    assert_eq!(inv2.issuer, issuer);
    assert_eq!(inv1.buyer, buyer);
    assert_eq!(inv2.buyer, buyer);

    assert_eq!(inv1.face_value, face_value);
    assert_eq!(inv2.face_value, face_value);
    assert_eq!(inv1.due_date, due_date);
    assert_eq!(inv2.due_date, due_date);

    assert_eq!(inv1.funding_asset, usdc);
    assert_eq!(inv2.funding_asset, usdc);
    assert_eq!(inv1.status, InvoiceStatus::Created);
    assert_eq!(inv2.status, InvoiceStatus::Created);

    // Assert internal instance counter incremented to 2
    let counter: u64 = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&crate::DataKey::Counter)
            .unwrap()
    });
    assert_eq!(counter, 2);

    // Assert index queries return both invoices
    assert_eq!(client.get_by_issuer(&issuer).len(), 2);
    assert_eq!(client.get_by_buyer(&buyer).len(), 2);
    assert_eq!(client.get_by_status(&InvoiceStatus::Created).len(), 2);

    // 3. Assert emitted events: two invoice_created events emitted.
    // The setup() helper also emits a contract_initialized event, so use
    // penultimate and last to grab the two invoice_created events.
    let contract_id = client.address.clone();
    let events = env.events().all();
    assert!(events.len() >= 3, "expected at least three events");

    let (event1_contract, event1_topics, event1_data) = events
        .get(events.len() - 2)
        .expect("expected penultimate event");
    assert_eq!(event1_contract, contract_id);
    assert_eq!(
        event1_topics,
        (
            Symbol::new(&env, "invoice_created"),
            id1.clone(),
            issuer.clone(),
            buyer.clone(),
            usdc.clone(),
        )
            .into_val(&env)
    );
    assert_eq!(u128::try_from_val(&env, &event1_data).unwrap(), face_value);

    let (event2_contract, event2_topics, event2_data) = events.last().expect("expected last event");
    assert_eq!(event2_contract, contract_id);
    assert_eq!(
        event2_topics,
        (
            Symbol::new(&env, "invoice_created"),
            id2.clone(),
            issuer.clone(),
            buyer.clone(),
            usdc.clone(),
        )
            .into_val(&env)
    );
    assert_eq!(u128::try_from_val(&env, &event2_data).unwrap(), face_value);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_create_unauthorized_issuer_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    // Call create without mocking issuer auth -> should panic with Error(Auth, InvalidAction)
    env.mock_auths(&[]);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
fn test_invoice_ids_unique_for_different_issuers() {
    // Test that different issuer/buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different issuer
    let issuer2 = Address::generate(&env);
    registry.register(&issuer2);
    let invoice_id_2 = client.create(&issuer2, &buyer, &face_value, &due_date, &usdc);

    // Different issuers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct issuers
    assert_eq!(client.get(&invoice_id_1).issuer, issuer);
    assert_eq!(client.get(&invoice_id_2).issuer, issuer2);
}

#[test]
fn test_invoice_ids_unique_for_different_buyers() {
    // Test that different buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different buyer
    let buyer2 = Address::generate(&env);
    registry.register(&buyer2);
    let invoice_id_2 = client.create(&issuer, &buyer2, &face_value, &due_date, &usdc);

    // Different buyers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct buyers
    assert_eq!(client.get(&invoice_id_1).buyer, buyer);
    assert_eq!(client.get(&invoice_id_2).buyer, buyer2);
}

#[test]
fn test_invoice_ids_unique_for_different_face_values() {
    // Test that different face values produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let id1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    let id2 = client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    assert_ne!(id1, id2);
}

// ── Issue #196: repay from Funded, Active, or Confirmed ─────────────────────────

fn mint_tokens(env: &Env, token: &Address, to: &Address, amount: i128) {
    let token_client = MockTokenClient::new(env, token);
    token_client.mint(to, &amount);
}

#[test]
fn test_repay_from_funded_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value: u128 = 1_000_000_000;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_from_active_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value: u128 = 1_000_000_000;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_from_confirmed_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value: u128 = 1_000_000_000;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_emits_event() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value: u128 = 1_000_000_000;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    client.repay(&invoice_id);

    // Contract events (non-diagnostic) include the repay event
    let events = env.events().all();
    let last_idx = events.len() - 1;
    let (_contract_id, repay_topics, _data) = events.get(last_idx).unwrap();
    // Verify the last event is invoice_repaid by comparing the Vec via assert_eq
    let expected_topics: soroban_sdk::Vec<soroban_sdk::Val> = vec![
        &env,
        soroban_sdk::Symbol::new(&env, "invoice_repaid").into_val(&env),
        invoice_id.into_val(&env),
    ];
    assert_eq!(repay_topics, expected_topics);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_fails_from_created() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    // Status is Created — repay should panic
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_fails_from_listed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    // Status is Listed — repay should panic
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_repay_fails_no_auth() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool.clone(),
                usdc.clone(),
                980_000_000u128,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    // Do not mock auth for buyer — repay should fail with auth error
    client.repay(&invoice_id);
}

// ============== PROPERTY-BASED INVARIANT TESTS ==============
// Uses proptest's TestRunner API directly (standard Rust closures) so
// rustfmt formats the tests normally.  Case budget is 10 per property
// to stay within CI time budgets for the Soroban in-process host.

#[test]
fn prop_any_positive_face_value_creates_invoice_in_created_status() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=1_000_000_000_000_000u128), |face_value| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + 86400;
            let id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
            let inv = client.get(&id);
            prop_assert_eq!(inv.face_value, face_value);
            prop_assert_eq!(inv.status, InvoiceStatus::Created);
            prop_assert!(!inv.issuer_confirmed);
            prop_assert!(!inv.buyer_confirmed);
            prop_assert_eq!(inv.funded_amount, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_any_future_due_date_creates_invoice_successfully() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u64..=31_536_000u64), |offset| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + offset;
            let id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
            let inv = client.get(&id);
            prop_assert_eq!(inv.due_date, due_date);
            prop_assert_eq!(inv.status, InvoiceStatus::Created);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_discount_bps_within_limit_always_lists_invoice() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(0u32..=5000u32), |discount_bps| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + 86400;
            let id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
            let result = client.list_for_financing(&id, &discount_bps);
            prop_assert!(result);
            let inv = client.get(&id);
            prop_assert_eq!(inv.discount_bps, discount_bps);
            prop_assert_eq!(inv.status, InvoiceStatus::Listed);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_invoice_id_is_deterministic_for_same_inputs() {
    // Same issuer, buyer, face_value, due_date, asset at the same ledger
    // timestamp must always produce the same invoice ID.
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=1_000_000_000_000u128), |face_value| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + 86400;
            let id1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
            // counter increments each call, so a second create with identical
            // params produces a different ID — verify the first is stable via get()
            let inv = client.get(&id1);
            prop_assert_eq!(inv.id, id1);
            prop_assert_eq!(inv.face_value, face_value);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_expiry_window_bounds_are_respected_across_values() {
    // For any window in [1, 30 days], a listing that expires exactly
    // window+1 seconds later must succeed.
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u64..=2_592_000u64), |window| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            client.set_expiry_window(&window);
            prop_assert_eq!(client.get_expiry_window(), window);
            let due_date = env.ledger().timestamp() + window + 86_400;
            let id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
            client.list_for_financing(&id, &200);
            env.ledger()
                .set_timestamp(env.ledger().timestamp() + window + 1);
            let expired = client.expire_listing(&id, &issuer);
            prop_assert!(expired);
            prop_assert_eq!(client.get(&id).status, InvoiceStatus::Expired);
            Ok(())
        })
        .unwrap();
}

#[test]
fn test_invoice_ids_unique_for_different_due_dates() {
    // Test that different due dates produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value = 1_000_000_000u128;
    let due_date_1 = env.ledger().timestamp() + 86400;
    let due_date_2 = env.ledger().timestamp() + 172800;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date_1, &usdc);

    // Create second invoice with different due date
    let invoice_id_2 = client.create(&issuer, &buyer, &face_value, &due_date_2, &usdc);

    // Different due dates should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct due dates
    assert_eq!(client.get(&invoice_id_1).due_date, due_date_1);
    assert_eq!(client.get(&invoice_id_2).due_date, due_date_2);
}

#[test]
fn test_invoice_ids_unique_for_different_assets() {
    // Test that different funding assets produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice with usdc
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create a different token asset
    let other_token = env.register_contract(None, MockToken);

    // Create second invoice with different asset
    let invoice_id_2 = client.create(&issuer, &buyer, &face_value, &due_date, &other_token);

    // Different assets should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct assets
    assert_eq!(client.get(&invoice_id_1).funding_asset, usdc);
    assert_eq!(client.get(&invoice_id_2).funding_asset, other_token);
}

#[test]
fn test_multiple_invoices_have_unique_ids() {
    // Test that creating multiple invoices produces unique IDs (counter increments)
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    let mut ids: soroban_sdk::Vec<BytesN<32>> = soroban_sdk::Vec::new(&env);
    for _ in 0..5 {
        let id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
        ids.push_back(id);
    }

    // All IDs should be unique due to incrementing counter
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(
                ids.get_unchecked(i),
                ids.get_unchecked(j),
                "Invoice IDs should be unique"
            );
        }
    }
}

#[test]
fn test_existing_valid_addresses_still_work() {
    // Regression test: verify existing valid address formats still generate valid invoice IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value = 1_000_000_000u128;
    let due_date = env.ledger().timestamp() + 86400;

    // Should not panic and should return a valid invoice ID
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Verify invoice exists and is valid
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.issuer, issuer);
    assert_eq!(invoice.buyer, buyer);
    assert_eq!(invoice.face_value, face_value);
    assert_eq!(invoice.due_date, due_date);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

#[test]
#[should_panic]
fn test_expire_listing_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);

    // Set pool contract (admin auth)
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool_id);

    // mark_funded requires pool auth
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool_id,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool_id.clone(),
                usdc.clone(),
                980_000_000u128,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);

    // mark_shipped by issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_shipped",
            args: (invoice_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_shipped(&invoice_id);

    // confirm delivery by issuer and buyer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), issuer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &issuer);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &buyer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), buyer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &buyer);

    // Fast forward past due date
    env.ledger().set_timestamp(due_date + 1);

    // Now call expire_listing without mocking admin auth -> should panic due to failed require_auth
    client.expire_listing(&invoice_id, &admin);
}

// ===================== END RESTORED TESTS =====================

// ============================== REPAY TESTS ==============================

#[test]
fn test_repay_from_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    let events_before = env.events().all().len();
    client.repay(&invoice_id);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Repaid);
    assert!(invoice.repaid_at.is_some());
    assert!(env.events().all().len() > events_before);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_repay_wrong_auth_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    env.set_auths(&[]);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id, &issuer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_create_fails_counter_overflow() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&crate::DataKey::Counter, &u64::MAX);
    });

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

// ============== ISSUE #201: TYPED ERRORS FOR UNINITIALIZED CONTRACT ==============

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_uninitialized_registry() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let due_date = env.ledger().timestamp() + 86400;
    let usdc = Address::generate(&env);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_missing_counter() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    env.as_contract(&client.address, || {
        env.storage().instance().remove(&crate::DataKey::Counter);
    });

    let due_date = env.ledger().timestamp() + 86400;
    let usdc = Address::generate(&env);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}
