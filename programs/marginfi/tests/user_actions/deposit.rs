use fixed::types::I80F48;
use fixtures::prelude::*;
use fixtures::{assert_custom_error, native};
use marginfi::state::marginfi_group::{BankConfigOpt, BankVaultType};
use marginfi::{assert_eq_with_tolerance, prelude::*};
use pretty_assertions::assert_eq;
use test_case::test_case;

use solana_program_test::*;

#[test_case(0.0, BankMint::Usdc)]
#[test_case(0.05, BankMint::UsdcSwb)]
#[test_case(1_000.0, BankMint::Usdc)]
#[test_case(0.05, BankMint::Sol)]
#[test_case(15_002.0, BankMint::SolSwb)]
#[test_case(0.05, BankMint::PyUSD)]
#[test_case(15_002.0, BankMint::PyUSD)]
#[test_case(0.0, BankMint::T22WithFee)]
#[test_case(0.05, BankMint::T22WithFee)]
#[test_case(15_002.0, BankMint::T22WithFee)]
#[tokio::test]
async fn marginfi_account_deposit_success(
    deposit_amount: f64,
    bank_mint: BankMint,
) -> anyhow::Result<()> {
    let mut test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;
    let user_wallet_balance = get_max_deposit_amount_pre_fee(deposit_amount);

    let marginfi_account_f = test_f.create_marginfi_account().await;
    let token_account_f = TokenAccountFixture::new(
        test_f.context.clone(),
        &test_f.get_bank(&bank_mint).mint,
        &test_f.payer(),
    )
    .await;

    let bank_f = test_f.get_bank_mut(&bank_mint);

    bank_f
        .mint
        .mint_to(&token_account_f.key, user_wallet_balance)
        .await;

    let pre_vault_balance = bank_f
        .get_vault_token_account(BankVaultType::Liquidity)
        .await
        .balance()
        .await;

    let res = marginfi_account_f
        .try_bank_deposit(token_account_f.key, &bank_f, deposit_amount)
        .await;
    assert!(res.is_ok());

    let post_vault_balance = bank_f
        .get_vault_token_account(BankVaultType::Liquidity)
        .await
        .balance()
        .await;

    let marginfi_account = marginfi_account_f.load().await;
    let active_balance_count = marginfi_account
        .lending_account
        .get_active_balances_iter()
        .count();
    assert_eq!(1, active_balance_count);

    let maybe_balance = marginfi_account.lending_account.get_balance(&bank_f.key);
    assert!(maybe_balance.is_some());

    let balance = maybe_balance.unwrap();

    let expected = I80F48::from(native!(deposit_amount, bank_f.mint.mint.decimals, f64));
    let actual = I80F48::from(post_vault_balance - pre_vault_balance);
    let accounted = bank_f
        .load()
        .await
        .get_asset_amount(balance.asset_shares.into())
        .unwrap();
    assert_eq!(expected, actual);
    assert_eq_with_tolerance!(expected, accounted, 1);

    Ok(())
}

#[test_case(1_000., 456., 2345., BankMint::Usdc)]
#[test_case(1_000., 456., 2345., BankMint::UsdcSwb)]
#[test_case(1_000., 456., 2345., BankMint::Sol)]
#[test_case(1_000., 456., 2345., BankMint::SolSwb)]
#[test_case(1_000., 456., 2345., BankMint::PyUSD)]
#[test_case(1_000., 456., 2345., BankMint::T22WithFee)]
#[test_case(1_000., 999.999999, 1000., BankMint::T22WithFee)]
#[tokio::test]
async fn marginfi_account_deposit_failure_capacity_exceeded(
    deposit_cap: f64,
    deposit_amount_ok: f64,
    deposit_amount_failed: f64,
    bank_mint: BankMint,
) -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;
    let user_wallet_balance = get_max_deposit_amount_pre_fee(deposit_amount_failed);
    let bank_f = test_f.get_bank(&bank_mint);

    bank_f
        .update_config(BankConfigOpt {
            deposit_limit: Some(native!(deposit_cap, bank_f.mint.mint.decimals, f64)),
            ..Default::default()
        })
        .await?;

    // Fund user account
    let user_mfi_account_f = test_f.create_marginfi_account().await;
    let user_token_account = bank_f
        .mint
        .create_token_account_and_mint_to(user_wallet_balance)
        .await;

    let res = user_mfi_account_f
        .try_bank_deposit(user_token_account.key, bank_f, deposit_amount_failed)
        .await;
    assert_custom_error!(res.unwrap_err(), MarginfiError::BankAssetCapacityExceeded);

    let res = user_mfi_account_f
        .try_bank_deposit(user_token_account.key, bank_f, deposit_amount_ok)
        .await;
    assert!(res.is_ok());

    Ok(())
}
