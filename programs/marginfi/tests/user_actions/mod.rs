mod borrow;
mod create_account;
mod deposit;
mod repay;
mod withdraw;

use std::{fs::File, io::Read, path::PathBuf, str::FromStr};

use anchor_lang::{prelude::Clock, AccountDeserialize, InstructionData, ToAccountMetas};
use anyhow::bail;
use base64::{engine::general_purpose::STANDARD, Engine};
use fixed::types::I80F48;
use fixed_macro::types::I80F48;
use fixtures::{assert_custom_error, assert_eq_noise, native, prelude::*};
use marginfi::{
    assert_eq_with_tolerance,
    constants::{
        EMISSIONS_FLAG_BORROW_ACTIVE, EMISSIONS_FLAG_LENDING_ACTIVE, MIN_EMISSIONS_START_TIME,
    },
    prelude::*,
    state::{
        marginfi_account::{
            BankAccountWrapper, MarginfiAccount, DISABLED_FLAG, FLASHLOAN_ENABLED_FLAG,
            IN_FLASHLOAN_FLAG, TRANSFER_AUTHORITY_ALLOWED_FLAG,
        },
        marginfi_group::{Bank, BankConfig, BankConfigOpt, BankVaultType},
    },
};
use pretty_assertions::assert_eq;
use solana_account_decoder::UiAccountData;
use solana_cli_output::CliAccount;
use solana_program::{instruction::Instruction, pubkey, pubkey::Pubkey};
use solana_program_test::*;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, signature::Keypair, signer::Signer,
    timing::SECONDS_PER_YEAR, transaction::Transaction,
};

#[tokio::test]
async fn marginfi_account_liquidation_success() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings {
        banks: vec![
            TestBankSetting {
                mint: BankMint::Usdc,
                ..TestBankSetting::default()
            },
            TestBankSetting {
                mint: BankMint::Sol,
                config: Some(BankConfig {
                    asset_weight_init: I80F48!(1).into(),
                    asset_weight_maint: I80F48!(1).into(),
                    ..*DEFAULT_SOL_TEST_BANK_CONFIG
                }),
            },
        ],
        group_config: Some(GroupConfig { admin: None }),
    }))
    .await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(2_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 2_000)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;

    // Borrower deposits 100 SOL worth of $1000
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 100)
        .await?;

    // Borrower borrows $999
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 999)
        .await?;

    // Synthetically bring down the borrower account health by reducing the asset weights of the SOL bank
    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.5).into()),
            ..Default::default()
        })
        .await?;

    lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await?;

    // Checks
    let sol_bank: Bank = sol_bank_f.load().await;
    let usdc_bank: Bank = usdc_bank_f.load().await;

    let depositor_ma = lender_mfi_account_f.load().await;
    let borrower_ma = borrower_mfi_account_f.load().await;

    // Depositors should have 1 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(depositor_ma.lending_account.balances[1].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1, "SOL"))
    );

    // Depositors should have 1990.25 USDC
    assert_eq_noise!(
        usdc_bank
            .get_asset_amount(depositor_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1990.25, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Borrower should have 99 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(borrower_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(99, "SOL"))
    );

    // Borrower should have 989.50 USDC
    assert_eq_noise!(
        usdc_bank
            .get_liability_amount(
                borrower_ma.lending_account.balances[1]
                    .liability_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(989.50, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Check insurance fund fee
    let insurance_fund_usdc = usdc_bank_f
        .get_vault_token_account(BankVaultType::Insurance)
        .await;

    assert_eq_noise!(
        insurance_fund_usdc.balance().await as i64,
        native!(0.25, "USDC", f64) as i64,
        1
    );

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_success_many_balances() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::many_banks_10())).await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);
    let sol_eq_bank_f = test_f.get_bank(&BankMint::SolEquivalent);
    let sol_eq1_bank_f = test_f.get_bank(&BankMint::SolEquivalent1);
    let sol_eq2_bank_f = test_f.get_bank(&BankMint::SolEquivalent2);
    let sol_eq3_bank_f = test_f.get_bank(&BankMint::SolEquivalent3);
    let sol_eq4_bank_f = test_f.get_bank(&BankMint::SolEquivalent4);
    let sol_eq5_bank_f = test_f.get_bank(&BankMint::SolEquivalent5);
    let sol_eq6_bank_f = test_f.get_bank(&BankMint::SolEquivalent6);
    let sol_eq7_bank_f = test_f.get_bank(&BankMint::SolEquivalent7);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(2_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 2_000)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;
    let borrower_token_account_sol_eq = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(5000)
        .await;

    // Borrower deposits 100 SOL worth of $1000
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 100)
        .await?;

    // Borrower borrows $999
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 999)
        .await?;

    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq1_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq2_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq3_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq4_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq5_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq6_bank_f, 0)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq7_bank_f, 0)
        .await?;

    // Synthetically bring down the borrower account health by reducing the asset weights of the SOL bank
    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.5).into()),
            ..Default::default()
        })
        .await?;

    lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await?;

    // Checks
    let sol_bank: Bank = sol_bank_f.load().await;
    let usdc_bank: Bank = usdc_bank_f.load().await;

    let depositor_ma = lender_mfi_account_f.load().await;
    let borrower_ma = borrower_mfi_account_f.load().await;

    // Depositors should have 1 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(depositor_ma.lending_account.balances[1].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1, "SOL"))
    );

    // Depositors should have 1990.25 USDC
    assert_eq_noise!(
        usdc_bank
            .get_asset_amount(depositor_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1990.25, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Borrower should have 99 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(borrower_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(99, "SOL"))
    );

    // Borrower should have 989.50 USDC
    assert_eq_noise!(
        usdc_bank
            .get_liability_amount(
                borrower_ma.lending_account.balances[1]
                    .liability_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(989.50, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Check insurance fund fee
    let insurance_fund_usdc = usdc_bank_f
        .get_vault_token_account(BankVaultType::Insurance)
        .await;

    assert_eq_noise!(
        insurance_fund_usdc.balance().await as i64,
        native!(0.25, "USDC", f64) as i64,
        1
    );

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_success_swb() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings {
        banks: vec![
            TestBankSetting {
                mint: BankMint::Usdc,
                ..TestBankSetting::default()
            },
            TestBankSetting {
                mint: BankMint::Sol,
                config: Some(BankConfig {
                    asset_weight_init: I80F48!(1).into(),
                    asset_weight_maint: I80F48!(1).into(),
                    ..*DEFAULT_SOL_TEST_SW_BANK_CONFIG
                }),
            },
        ],
        group_config: Some(GroupConfig { admin: None }),
    }))
    .await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(2_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 2_000)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;

    // Borrower deposits 100 SOL worth of $1000
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 100)
        .await?;

    // Borrower borrows $999
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 999)
        .await?;

    // Synthetically bring down the borrower account health by reducing the asset weights of the SOL bank
    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.5).into()),
            ..Default::default()
        })
        .await?;

    lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await?;

    // Checks
    let sol_bank: Bank = sol_bank_f.load().await;
    let usdc_bank: Bank = usdc_bank_f.load().await;

    let depositor_ma = lender_mfi_account_f.load().await;
    let borrower_ma = borrower_mfi_account_f.load().await;

    // Depositors should have 1 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(depositor_ma.lending_account.balances[1].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1, "SOL"))
    );

    // Depositors should have 1990.25 USDC
    assert_eq_noise!(
        usdc_bank
            .get_asset_amount(depositor_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(1990.25, "USDC", f64)),
        native!(0.01, "USDC", f64)
    );

    // Borrower should have 99 SOL
    assert_eq!(
        sol_bank
            .get_asset_amount(borrower_ma.lending_account.balances[0].asset_shares.into())
            .unwrap(),
        I80F48::from(native!(99, "SOL"))
    );

    // Borrower should have 989.50 USDC
    assert_eq_noise!(
        usdc_bank
            .get_liability_amount(
                borrower_ma.lending_account.balances[1]
                    .liability_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(989.50, "USDC", f64)),
        native!(0.01, "USDC", f64)
    );

    // Check insurance fund fee
    let insurance_fund_usdc = usdc_bank_f
        .get_vault_token_account(BankVaultType::Insurance)
        .await;

    assert_eq_noise!(
        insurance_fund_usdc.balance().await as i64,
        native!(0.25, "USDC", f64) as i64,
        native!(0.001, "USDC", f64) as i64
    );

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_failure_liquidatee_not_unhealthy() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings {
        banks: vec![
            TestBankSetting {
                mint: BankMint::Usdc,
                config: Some(BankConfig {
                    asset_weight_maint: I80F48!(1).into(),
                    ..*DEFAULT_USDC_TEST_BANK_CONFIG
                }),
            },
            TestBankSetting {
                mint: BankMint::Sol,
                config: Some(BankConfig {
                    asset_weight_init: I80F48!(1).into(),
                    asset_weight_maint: I80F48!(1).into(),
                    ..*DEFAULT_SOL_TEST_BANK_CONFIG
                }),
            },
        ],
        group_config: Some(GroupConfig { admin: None }),
    }))
    .await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f.usdc_mint.create_token_account_and_mint_to(200).await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 200)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;

    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 100)
        .await?;

    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 100)
        .await?;

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await;

    assert!(res.is_err());

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalLiquidation);

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_failure_liquidation_too_severe() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f.usdc_mint.create_token_account_and_mint_to(200).await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 200)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(10).await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 10)
        .await?;
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 61)
        .await?;

    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.5).into()),
            ..Default::default()
        })
        .await?;

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 10, usdc_bank_f)
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalLiquidation);

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await;

    assert!(res.is_ok());

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_failure_liquidator_no_collateral() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings {
        banks: vec![
            TestBankSetting {
                mint: BankMint::Usdc,
                config: Some(BankConfig {
                    liability_weight_init: I80F48!(1.2).into(),
                    liability_weight_maint: I80F48!(1.1).into(),
                    ..*DEFAULT_USDC_TEST_BANK_CONFIG
                }),
            },
            TestBankSetting {
                mint: BankMint::Sol,
                config: None,
            },
            TestBankSetting {
                mint: BankMint::SolEquivalent,
                config: None,
            },
        ],
        group_config: Some(GroupConfig { admin: None }),
    }))
    .await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);
    let sol_eq_bank_f = test_f.get_bank(&BankMint::SolEquivalent);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f.usdc_mint.create_token_account_and_mint_to(200).await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 200)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_sol_eq = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(100)
        .await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 10)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq_bank_f, 1)
        .await?;
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 60)
        .await?;

    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.3).into()),
            ..Default::default()
        })
        .await?;

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_eq_bank_f, 2, usdc_bank_f)
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalLiquidation);

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_eq_bank_f, 1, usdc_bank_f)
        .await;

    assert!(res.is_ok());

    Ok(())
}

#[tokio::test]
async fn marginfi_account_liquidation_failure_bank_not_liquidatable() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);
    let sol_eq_bank_f = test_f.get_bank(&BankMint::SolEquivalent);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f.usdc_mint.create_token_account_and_mint_to(200).await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 200)
        .await?;

    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_sol = test_f.sol_mint.create_token_account_and_mint_to(100).await;
    let borrower_token_account_sol_eq = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(100)
        .await;
    let borrower_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol.key, sol_bank_f, 10)
        .await?;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_sol_eq.key, sol_eq_bank_f, 1)
        .await?;
    borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_usdc.key, usdc_bank_f, 60)
        .await?;

    sol_bank_f
        .update_config(BankConfigOpt {
            asset_weight_init: Some(I80F48!(0.25).into()),
            asset_weight_maint: Some(I80F48!(0.4).into()),
            ..Default::default()
        })
        .await?;

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_eq_bank_f, 1, sol_bank_f)
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalLiquidation);

    let res = lender_mfi_account_f
        .try_liquidate(&borrower_mfi_account_f, sol_bank_f, 1, usdc_bank_f)
        .await;

    assert!(res.is_ok());

    Ok(())
}

#[tokio::test]
async fn automatic_interest_payments() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    // Create lender user accounts and deposit SOL asset
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_bank_f, 1_000)
        .await?;

    // Create borrower user accounts and deposit USDC asset
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_usdc.key, usdc_bank_f, 1_000)
        .await?;

    // Borrow SOL from borrower mfi account
    borrower_mfi_account_f
        .try_bank_borrow(lender_token_account_sol.key, sol_bank_f, 99)
        .await?;

    // Let a year go by
    {
        let mut ctx = test_f.context.borrow_mut();
        let mut clock: Clock = ctx.banks_client.get_sysvar().await?;
        // Advance clock by 1 year
        clock.unix_timestamp += 365 * 24 * 60 * 60;
        ctx.set_sysvar(&clock);
    }

    // Repay principal, leaving only the accrued interest
    borrower_mfi_account_f
        .try_bank_repay(lender_token_account_sol.key, sol_bank_f, 99, None)
        .await?;

    let sol_bank = sol_bank_f.load().await;
    let borrower_mfi_account = borrower_mfi_account_f.load().await;
    let lender_mfi_account = lender_mfi_account_f.load().await;

    // Verify that interest accrued matches on both sides
    assert_eq_noise!(
        sol_bank
            .get_liability_amount(
                borrower_mfi_account.lending_account.balances[1]
                    .liability_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(11.761, "SOL", f64)),
        native!(0.0002, "SOL", f64)
    );

    assert_eq_noise!(
        sol_bank
            .get_asset_amount(
                lender_mfi_account.lending_account.balances[0]
                    .asset_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(1011.761, "SOL", f64)),
        native!(0.0002, "SOL", f64)
    );
    // TODO: check health is sane

    Ok(())
}

// Regression

#[tokio::test]
async fn marginfi_account_correct_balance_selection_after_closing_position() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let usdc_bank_f = test_f.get_bank(&BankMint::Usdc);
    let sol_bank_f = test_f.get_bank(&BankMint::Sol);

    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_bank_f, 1_000)
        .await?;
    let lender_token_account_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(2_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank_f, 2_000)
        .await?;

    lender_mfi_account_f
        .try_bank_withdraw(lender_token_account_sol.key, sol_bank_f, 0, Some(true))
        .await
        .unwrap();

    let mut marginfi_account = lender_mfi_account_f.load().await;
    let mut usdc_bank = usdc_bank_f.load().await;

    let bank_account = BankAccountWrapper::find(
        &usdc_bank_f.key,
        &mut usdc_bank,
        &mut marginfi_account.lending_account,
    );

    assert!(bank_account.is_ok());

    let bank_account = bank_account.unwrap();

    assert_eq!(
        bank_account
            .bank
            .get_asset_amount(bank_account.balance.asset_shares.into())
            .unwrap()
            .to_num::<u64>(),
        native!(2_000, "USDC")
    );

    Ok(())
}

#[tokio::test]
async fn isolated_borrows() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_one_isolated())).await;

    let usdc_bank = test_f.get_bank(&BankMint::Usdc);
    let sol_eq_bank = test_f.get_bank(&BankMint::SolEquivalent);
    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_sol = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_eq_bank, 1_000)
        .await?;

    let lender_token_account_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_f_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    let borrower_token_account_f_sol = test_f
        .sol_equivalent_mint
        .create_empty_token_account()
        .await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_f_usdc.key, usdc_bank, 1_000)
        .await?;

    // Borrow SOL EQ
    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_f_sol.key, sol_eq_bank, 10)
        .await;

    assert!(res.is_ok());

    // Repay isolated SOL EQ borrow and borrow SOL successfully,
    let borrower_sol_account = test_f.sol_mint.create_empty_token_account().await;
    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_sol_account.key, sol_bank, 10)
        .await;

    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IsolatedAccountIllegalState);

    borrower_mfi_account_f
        .try_bank_repay(borrower_token_account_f_sol.key, sol_eq_bank, 0, Some(true))
        .await?;

    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_sol_account.key, sol_bank, 10)
        .await;

    assert!(res.is_ok());

    // Borrowing SOL EQ again fails
    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_f_sol.key, sol_eq_bank, 10)
        .await;

    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IsolatedAccountIllegalState);

    Ok(())
}

#[tokio::test]
async fn emissions_test() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_one_isolated())).await;

    let usdc_bank = test_f.get_bank(&BankMint::Usdc);
    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Setup emissions (Deposit for USDC, Borrow for SOL)

    let funding_account = test_f.usdc_mint.create_token_account_and_mint_to(100).await;

    usdc_bank
        .try_setup_emissions(
            EMISSIONS_FLAG_LENDING_ACTIVE,
            1_000_000,
            native!(50, "USDC"),
            usdc_bank.mint.key,
            funding_account.key,
            usdc_bank.get_token_program(),
        )
        .await?;

    // SOL Emissions are not in SOL Bank mint
    let sol_emissions_mint =
        MintFixture::new_token_22(test_f.context.clone(), None, Some(6), &[]).await;

    let funding_account = sol_emissions_mint
        .create_token_account_and_mint_to(200)
        .await;

    sol_bank
        .try_setup_emissions(
            EMISSIONS_FLAG_BORROW_ACTIVE,
            1_000_000,
            native!(100, 6),
            sol_emissions_mint.key,
            funding_account.key,
            sol_emissions_mint.token_program,
        )
        .await?;

    let sol_emissions_mint_2 =
        MintFixture::new_token_22(test_f.context.clone(), None, Some(6), &[]).await;

    let funding_account = sol_emissions_mint_2
        .create_token_account_and_mint_to(200)
        .await;

    let res = sol_bank
        .try_setup_emissions(
            EMISSIONS_FLAG_BORROW_ACTIVE,
            1_000_000,
            native!(50, 6),
            sol_emissions_mint_2.key,
            funding_account.key,
            sol_emissions_mint_2.token_program,
        )
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::EmissionsAlreadySetup);

    // Fund SOL bank
    let sol_lender_account = test_f.create_marginfi_account().await;
    let sol_lender_token_account = test_f.sol_mint.create_token_account_and_mint_to(100).await;

    sol_lender_account
        .try_bank_deposit(sol_lender_token_account.key, sol_bank, 100)
        .await?;

    // Create account and setup positions
    test_f.set_time(MIN_EMISSIONS_START_TIME as i64);
    test_f
        .set_pyth_oracle_timestamp(PYTH_USDC_FEED, MIN_EMISSIONS_START_TIME as i64)
        .await;
    test_f
        .set_pyth_oracle_timestamp(PYTH_SOL_FEED, MIN_EMISSIONS_START_TIME as i64)
        .await;

    let mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_usdc = test_f.usdc_mint.create_token_account_and_mint_to(50).await;

    mfi_account_f
        .try_bank_deposit(lender_token_account_usdc.key, usdc_bank, 50)
        .await?;

    let sol_account = test_f.sol_mint.create_empty_token_account().await;

    mfi_account_f
        .try_bank_borrow(sol_account.key, sol_bank, 2)
        .await?;

    // Advance for half a year and claim half emissions
    test_f.advance_time((SECONDS_PER_YEAR / 2.0) as i64).await;

    let lender_token_account_usdc = test_f.usdc_mint.create_empty_token_account().await;

    mfi_account_f
        .try_withdraw_emissions(usdc_bank, &lender_token_account_usdc)
        .await?;

    let sol_emissions_ta = sol_emissions_mint.create_empty_token_account().await;

    mfi_account_f
        .try_withdraw_emissions(sol_bank, &sol_emissions_ta)
        .await?;

    assert_eq_with_tolerance!(
        lender_token_account_usdc.balance().await as i64,
        native!(25, "USDC") as i64,
        native!(1, "USDC") as i64
    );

    assert_eq_with_tolerance!(
        sol_emissions_ta.balance().await as i64,
        native!(1, 6) as i64,
        native!(0.1, 6, f64) as i64
    );

    // Advance for another half a year and claim the rest
    test_f.advance_time((SECONDS_PER_YEAR / 2.0) as i64).await;

    mfi_account_f
        .try_withdraw_emissions(usdc_bank, &lender_token_account_usdc)
        .await?;

    assert_eq_with_tolerance!(
        lender_token_account_usdc.balance().await as i64,
        native!(50, "USDC") as i64,
        native!(1, "USDC") as i64
    );

    mfi_account_f
        .try_withdraw_emissions(sol_bank, &sol_emissions_ta)
        .await?;

    assert_eq_with_tolerance!(
        sol_emissions_ta.balance().await as i64,
        native!(2, 6) as i64,
        native!(0.1, 6, f64) as i64
    );

    // Advance a year, and no more USDC emissions can be claimed (drained), SOL emissions can be claimed

    test_f.advance_time((SECONDS_PER_YEAR / 2.0) as i64).await;

    mfi_account_f
        .try_withdraw_emissions(usdc_bank, &lender_token_account_usdc)
        .await?;

    mfi_account_f
        .try_withdraw_emissions(sol_bank, &sol_emissions_ta)
        .await?;

    assert_eq_with_tolerance!(
        lender_token_account_usdc.balance().await as i64,
        native!(50, "USDC") as i64,
        native!(1, "USDC") as i64
    );

    assert_eq_with_tolerance!(
        sol_emissions_ta.balance().await as i64,
        native!(3, 6) as i64,
        native!(0.1, 6, f64) as i64
    );

    // SOL lendeing account can't claim emissions, bc SOL is borrow only emissions
    let sol_lender_emissions = sol_emissions_mint.create_empty_token_account().await;

    sol_lender_account
        .try_withdraw_emissions(sol_bank, &sol_lender_emissions)
        .await?;

    assert_eq!(sol_lender_emissions.balance().await as i64, 0);

    Ok(())
}

#[tokio::test]
async fn emissions_test_2() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_one_isolated())).await;

    let usdc_bank = test_f.get_bank(&BankMint::Usdc);

    let funding_account = test_f.usdc_mint.create_token_account_and_mint_to(100).await;

    usdc_bank
        .try_setup_emissions(
            EMISSIONS_FLAG_LENDING_ACTIVE,
            1_000_000,
            native!(50, "USDC"),
            usdc_bank.mint.key,
            funding_account.key,
            usdc_bank.get_token_program(),
        )
        .await?;

    let usdc_bank_data = usdc_bank.load().await;

    assert_eq!(usdc_bank_data.flags, EMISSIONS_FLAG_LENDING_ACTIVE);

    assert_eq!(usdc_bank_data.emissions_rate, 1_000_000);

    assert_eq!(
        I80F48::from(usdc_bank_data.emissions_remaining),
        I80F48::from_num(native!(50, "USDC"))
    );

    usdc_bank
        .try_update_emissions(
            Some(EMISSIONS_FLAG_BORROW_ACTIVE),
            Some(500_000),
            Some((native!(25, "USDC"), funding_account.key)),
            usdc_bank.get_token_program(),
        )
        .await?;

    let usdc_bank_data = usdc_bank.load().await;

    assert_eq!(usdc_bank_data.flags, EMISSIONS_FLAG_BORROW_ACTIVE);

    assert_eq!(usdc_bank_data.emissions_rate, 500_000);

    assert_eq!(
        I80F48::from(usdc_bank_data.emissions_remaining),
        I80F48::from_num(native!(75, "USDC"))
    );

    Ok(())
}

#[tokio::test]
async fn emissions_setup_t22_with_fee() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let collateral_mint = BankMint::T22WithFee;
    let bank_f = test_f.get_bank(&collateral_mint);

    let funding_account = bank_f.mint.create_token_account_and_mint_to(100).await;

    let emissions_vault = get_emissions_token_account_address(bank_f.key, bank_f.mint.key).0;

    let pre_vault_balance = 0;

    bank_f
        .try_setup_emissions(
            EMISSIONS_FLAG_LENDING_ACTIVE,
            1_000_000,
            native!(50, bank_f.mint.mint.decimals),
            bank_f.mint.key,
            funding_account.key,
            bank_f.get_token_program(),
        )
        .await?;

    let post_vault_balance = TokenAccountFixture::fetch(test_f.context.clone(), emissions_vault)
        .await
        .balance()
        .await;

    let bank = bank_f.load().await;

    assert_eq!(bank.flags, EMISSIONS_FLAG_LENDING_ACTIVE);

    assert_eq!(bank.emissions_rate, 1_000_000);

    assert_eq!(
        I80F48::from(bank.emissions_remaining),
        I80F48::from_num(native!(50, bank_f.mint.mint.decimals))
    );

    let expected_vault_balance_delta = native!(50, bank_f.mint.mint.decimals) as u64;
    let actual_vault_balance_delta = post_vault_balance - pre_vault_balance;
    assert_eq!(expected_vault_balance_delta, actual_vault_balance_delta);

    let pre_vault_balance = TokenAccountFixture::fetch(test_f.context.clone(), emissions_vault)
        .await
        .balance()
        .await;

    bank_f
        .try_update_emissions(
            Some(EMISSIONS_FLAG_BORROW_ACTIVE),
            Some(500_000),
            Some((native!(25, bank_f.mint.mint.decimals), funding_account.key)),
            bank_f.get_token_program(),
        )
        .await?;

    let post_vault_balance = TokenAccountFixture::fetch(test_f.context.clone(), emissions_vault)
        .await
        .balance()
        .await;

    let bank_data = bank_f.load().await;

    assert_eq!(bank_data.flags, EMISSIONS_FLAG_BORROW_ACTIVE);

    assert_eq!(bank_data.emissions_rate, 500_000);

    assert_eq!(
        I80F48::from(bank_data.emissions_remaining),
        I80F48::from_num(native!(75, bank_f.mint.mint.decimals))
    );

    let expected_vault_balance_delta = native!(25, bank_f.mint.mint.decimals) as u64;
    let actual_vault_balance_delta = post_vault_balance - pre_vault_balance;
    assert_eq!(expected_vault_balance_delta, actual_vault_balance_delta);

    Ok(())
}

#[tokio::test]
async fn account_flags() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let mfi_account_f = test_f.create_marginfi_account().await;

    mfi_account_f.try_set_flag(FLASHLOAN_ENABLED_FLAG).await?;

    let mfi_account_data = mfi_account_f.load().await;

    assert_eq!(mfi_account_data.account_flags, FLASHLOAN_ENABLED_FLAG);

    assert!(mfi_account_data.get_flag(FLASHLOAN_ENABLED_FLAG));

    mfi_account_f.try_unset_flag(FLASHLOAN_ENABLED_FLAG).await?;

    let mfi_account_data = mfi_account_f.load().await;

    assert_eq!(mfi_account_data.account_flags, 0);

    let res = mfi_account_f.try_set_flag(DISABLED_FLAG).await;

    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlag);

    let res = mfi_account_f.try_unset_flag(IN_FLASHLOAN_FLAG).await;

    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlag);

    Ok(())
}

// Flashloan tests
// 1. Flashloan success (1 action)
// 2. Flashloan success (3 actions)
// 3. Flashloan fails because of bad account health
// 4. Flashloan fails because of non whitelisted account
// 5. Flashloan fails because of missing `end_flashloan` ix
// 6. Flashloan fails because of invalid instructions sysvar
// 7. Flashloan fails because of invalid `end_flashloan` ix order
// 8. Flashloan fails because `end_flashloan` ix is for another account
// 9. Flashloan fails because account is already in a flashloan

#[tokio::test]
async fn flashloan_success_1op() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let repay_ix = borrower_mfi_account_f
        .make_bank_repay_ix(
            borrower_token_account_f_sol.key,
            sol_bank,
            1_000,
            Some(true),
        )
        .await;

    let flash_loan_result = borrower_mfi_account_f
        .try_flashloan(vec![borrow_ix, repay_ix], vec![], vec![])
        .await;

    assert!(flash_loan_result.is_ok());

    Ok(())
}

#[tokio::test]
async fn flashloan_success_3op() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;

    // Create borrow and repay instructions
    let mut ixs = Vec::new();
    for _ in 0..3 {
        let borrow_ix = borrower_mfi_account_f
            .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
            .await;
        ixs.push(borrow_ix);

        let repay_ix = borrower_mfi_account_f
            .make_bank_repay_ix(
                borrower_token_account_f_sol.key,
                sol_bank,
                1_000,
                Some(true),
            )
            .await;
        ixs.push(repay_ix);
    }

    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));

    let flash_loan_result = borrower_mfi_account_f
        .try_flashloan(ixs, vec![], vec![])
        .await;

    assert!(flash_loan_result.is_ok());

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_account_health() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let flash_loan_result = borrower_mfi_account_f
        .try_flashloan(vec![borrow_ix], vec![], vec![sol_bank.key])
        .await;

    assert_custom_error!(
        flash_loan_result.unwrap_err(),
        MarginfiError::RiskEngineInitRejected
    );

    Ok(())
}

#[tokio::test]
// Note: The flashloan flag is now deprecated
async fn flashloan_ok_missing_flag() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let repay_ix = borrower_mfi_account_f
        .make_bank_repay_ix(
            borrower_token_account_f_sol.key,
            sol_bank,
            1_000,
            Some(true),
        )
        .await;

    let flash_loan_result = borrower_mfi_account_f
        .try_flashloan(vec![borrow_ix, repay_ix], vec![], vec![])
        .await;

    assert!(flash_loan_result.is_ok());

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_missing_fe_ix() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let repay_ix = borrower_mfi_account_f
        .make_bank_repay_ix(
            borrower_token_account_f_sol.key,
            sol_bank,
            1_000,
            Some(true),
        )
        .await;

    let mut ixs = vec![borrow_ix, repay_ix];

    let start_ix = borrower_mfi_account_f
        .make_lending_account_start_flashloan_ix(ixs.len() as u64)
        .await;

    ixs.insert(0, start_ix);

    let mut ctx = test_f.context.borrow_mut();

    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey().clone()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );

    let res = ctx.banks_client.process_transaction(tx).await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlashloan);

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_missing_invalid_sysvar_ixs() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let repay_ix = borrower_mfi_account_f
        .make_bank_repay_ix(
            borrower_token_account_f_sol.key,
            sol_bank,
            1_000,
            Some(true),
        )
        .await;

    let mut ixs = vec![borrow_ix, repay_ix];

    let start_ix = Instruction {
        program_id: marginfi::id(),
        accounts: marginfi::accounts::LendingAccountStartFlashloan {
            marginfi_account: borrower_mfi_account_f.key,
            signer: test_f.context.borrow().payer.pubkey(),
            ixs_sysvar: Pubkey::default(),
        }
        .to_account_metas(Some(true)),
        data: marginfi::instruction::LendingAccountStartFlashloan {
            end_index: ixs.len() as u64 + 1,
        }
        .data(),
    };

    let end_ix = borrower_mfi_account_f
        .make_lending_account_end_flashloan_ix(vec![], vec![])
        .await;

    ixs.insert(0, start_ix);
    ixs.push(end_ix);

    let mut ctx = test_f.context.borrow_mut();

    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey().clone()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );

    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(res.is_err());

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_invalid_end_fl_order() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let mut ixs = vec![borrow_ix];

    let start_ix = borrower_mfi_account_f
        .make_lending_account_start_flashloan_ix(0)
        .await;

    let end_ix = borrower_mfi_account_f
        .make_lending_account_end_flashloan_ix(vec![], vec![])
        .await;

    ixs.insert(0, start_ix);
    ixs.insert(0, end_ix);

    let mut ctx = test_f.context.borrow_mut();

    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey().clone()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );

    let res = ctx.banks_client.process_transaction(tx).await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlashloan);

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_invalid_end_fl_different_m_account() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let mut ixs = vec![borrow_ix];

    let start_ix = borrower_mfi_account_f
        .make_lending_account_start_flashloan_ix(ixs.len() as u64 + 1)
        .await;

    let end_ix = lender_mfi_account_f
        .make_lending_account_end_flashloan_ix(vec![], vec![])
        .await;

    ixs.insert(0, start_ix);
    ixs.push(end_ix);

    let mut ctx = test_f.context.borrow_mut();

    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey().clone()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );

    let res = ctx.banks_client.process_transaction(tx).await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlashloan);

    Ok(())
}

#[tokio::test]
async fn flashloan_fail_already_in_flashloan() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_f_sol.key, sol_bank, 1_000)
        .await?;

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;

    borrower_mfi_account_f
        .try_set_flag(FLASHLOAN_ENABLED_FLAG)
        .await?;

    let borrower_token_account_f_sol = test_f.sol_mint.create_empty_token_account().await;
    // Borrow SOL

    let borrow_ix = borrower_mfi_account_f
        .make_bank_borrow_ix(borrower_token_account_f_sol.key, sol_bank, 1_000)
        .await;

    let mut ixs = vec![borrow_ix];

    let start_ix = borrower_mfi_account_f
        .make_lending_account_start_flashloan_ix(ixs.len() as u64 + 2)
        .await;

    let end_ix = borrower_mfi_account_f
        .make_lending_account_end_flashloan_ix(vec![], vec![])
        .await;

    ixs.insert(0, start_ix.clone());
    ixs.insert(0, start_ix.clone());
    ixs.push(end_ix);

    let mut ctx = test_f.context.borrow_mut();

    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey().clone()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );

    let res = ctx.banks_client.process_transaction(tx).await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalFlashloan);

    Ok(())
}

#[tokio::test]
async fn lending_account_close_balance() -> anyhow::Result<()> {
    let test_f = TestFixture::new(Some(TestSettings::all_banks_payer_not_admin())).await;

    let usdc_bank = test_f.get_bank(&BankMint::Usdc);
    let sol_eq_bank = test_f.get_bank(&BankMint::SolEquivalent);
    let sol_bank = test_f.get_bank(&BankMint::Sol);

    // Fund SOL lender
    let lender_mfi_account_f = test_f.create_marginfi_account().await;
    let lender_token_account_sol = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_eq_bank, 1_000)
        .await?;

    let lender_token_account_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    lender_mfi_account_f
        .try_bank_deposit(lender_token_account_sol.key, sol_bank, 1_000)
        .await?;

    let res = lender_mfi_account_f.try_balance_close(sol_bank).await;

    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalBalanceState);

    // Fund SOL borrower
    let borrower_mfi_account_f = test_f.create_marginfi_account().await;
    let borrower_token_account_f_usdc = test_f
        .usdc_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    let borrower_token_account_f_sol = test_f
        .sol_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    let borrower_token_account_f_sol_eq = test_f
        .sol_equivalent_mint
        .create_token_account_and_mint_to(1_000)
        .await;
    borrower_mfi_account_f
        .try_bank_deposit(borrower_token_account_f_usdc.key, usdc_bank, 1_000)
        .await?;

    // Borrow SOL EQ
    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_f_sol_eq.key, sol_eq_bank, 0.01)
        .await;

    assert!(res.is_ok());

    // Borrow SOL
    let res = borrower_mfi_account_f
        .try_bank_borrow(borrower_token_account_f_sol.key, sol_bank, 0.01)
        .await;

    assert!(res.is_ok());

    // This issue is not that bad, because the user can still borrow other assets (isolated liab < empty threshold)
    let res = borrower_mfi_account_f.try_balance_close(sol_bank).await;
    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::IllegalBalanceState);

    // Let a second go b
    {
        let mut ctx = test_f.context.borrow_mut();
        let mut clock: Clock = ctx.banks_client.get_sysvar().await?;
        // Advance clock by 1 second
        clock.unix_timestamp += 1;
        ctx.set_sysvar(&clock);
    }

    // Repay isolated SOL EQ borrow successfully
    let res = borrower_mfi_account_f
        .try_bank_repay(
            borrower_token_account_f_sol_eq.key,
            sol_eq_bank,
            0.01,
            Some(false),
        )
        .await;
    assert!(res.is_ok());

    // Liability share in balance is smaller than 0.0001, so repay all should fail
    let res = borrower_mfi_account_f
        .try_bank_repay(
            borrower_token_account_f_sol_eq.key,
            sol_eq_bank,
            1,
            Some(true),
        )
        .await;
    assert!(res.is_err());
    assert_custom_error!(res.unwrap_err(), MarginfiError::NoLiabilityFound);

    // This issue is not that bad, because the user can still borrow other assets (isolated liab < empty threshold)
    let res = borrower_mfi_account_f.try_balance_close(sol_eq_bank).await;
    assert!(res.is_ok());

    Ok(())
}

// Test transfer account authority.
// No transfer flag set -- tx should fail.
// Set the flag and try again -- tx should succeed.
// RUST_BACKTRACE=1 cargo test-bpf marginfi_account_authority_transfer_no_flag_set -- --exact
#[tokio::test]
async fn marginfi_account_authority_transfer_no_flag_set() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;
    // Default account with no flags set
    let marginfi_account = test_f.create_marginfi_account().await;
    let new_authority = Keypair::new().pubkey();

    let res = marginfi_account
        .try_transfer_account_authority(new_authority, None)
        .await;

    // Assert the response is an error due to the lack of the correct flag
    assert!(res.is_err());
    assert_custom_error!(
        res.unwrap_err(),
        MarginfiError::IllegalAccountAuthorityTransfer
    );

    // set the flag on the account
    marginfi_account
        .try_set_flag(TRANSFER_AUTHORITY_ALLOWED_FLAG)
        .await
        .unwrap();

    let new_authority_2 = Keypair::new().pubkey();
    let res = marginfi_account
        .try_transfer_account_authority(new_authority_2, None)
        .await;

    assert!(res.is_ok());

    Ok(())
}

#[tokio::test]
async fn marginfi_account_authority_transfer_not_account_owner() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;
    let marginfi_account = test_f.create_marginfi_account().await;
    let new_authority = Keypair::new().pubkey();
    let signer = Keypair::new();

    let res = marginfi_account
        .try_transfer_account_authority(new_authority, Some(signer))
        .await;

    // Assert the response is an error due to fact that a non-owner of the
    // acount attempted to initialize this account transfer
    assert!(res.is_err());

    Ok(())
}

#[tokio::test]
async fn account_field_values_reg() -> anyhow::Result<()> {
    let account_fixtures_path = "tests/fixtures/marginfi_account";

    // Sample 1

    let mut path = PathBuf::from_str(account_fixtures_path).unwrap();
    path.push("marginfi_account_sample_1.json");
    let mut file = File::open(&path).unwrap();
    let mut account_info_raw = String::new();
    file.read_to_string(&mut account_info_raw).unwrap();

    let account: CliAccount = serde_json::from_str(&account_info_raw).unwrap();
    let UiAccountData::Binary(data, _) = account.keyed_account.account.data else {
        bail!("Expecting Binary format for fixtures")
    };
    let account = MarginfiAccount::try_deserialize(&mut STANDARD.decode(data)?.as_slice())?;

    assert_eq!(
        account.authority,
        pubkey!("Dq7wypbedtaqQK9QqEFvfrxc4ppfRGXCeTVd7ee7n2jw")
    );

    let balance_1 = account.lending_account.balances[0];
    assert!(balance_1.active);
    assert_eq!(
        I80F48::from(balance_1.asset_shares),
        I80F48::from_str("1650216221.466876226897366").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.liability_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.emissions_outstanding),
        I80F48::from_str("0").unwrap()
    );

    let balance_2 = account.lending_account.balances[1];
    assert!(balance_2.active);
    assert_eq!(
        I80F48::from(balance_2.asset_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_2.liability_shares),
        I80F48::from_str("3806372611.588862122556122").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_2.emissions_outstanding),
        I80F48::from_str("0").unwrap()
    );

    // Sample 2

    let mut path = PathBuf::from_str(account_fixtures_path).unwrap();
    path.push("marginfi_account_sample_2.json");
    let mut file = File::open(&path).unwrap();
    let mut account_info_raw = String::new();
    file.read_to_string(&mut account_info_raw).unwrap();

    let account: CliAccount = serde_json::from_str(&account_info_raw).unwrap();
    let UiAccountData::Binary(data, _) = account.keyed_account.account.data else {
        bail!("Expecting Binary format for fixtures")
    };
    let account = MarginfiAccount::try_deserialize(&mut STANDARD.decode(data)?.as_slice())?;

    assert_eq!(
        account.authority,
        pubkey!("3T1kGHp7CrdeW9Qj1t8NMc2Ks233RyvzVhoaUPWoBEFK")
    );

    let balance_1 = account.lending_account.balances[0];
    assert!(balance_1.active);
    assert_eq!(
        I80F48::from(balance_1.asset_shares),
        I80F48::from_str("470.952530958931234").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.liability_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.emissions_outstanding),
        I80F48::from_str("26891413.388324654086347").unwrap()
    );

    let balance_2 = account.lending_account.balances[1];
    assert!(!balance_2.active);
    assert_eq!(
        I80F48::from(balance_2.asset_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_2.liability_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_2.emissions_outstanding),
        I80F48::from_str("0").unwrap()
    );

    // Sample 3

    let mut path = PathBuf::from_str(account_fixtures_path).unwrap();
    path.push("marginfi_account_sample_3.json");
    let mut file = File::open(&path).unwrap();
    let mut account_info_raw = String::new();
    file.read_to_string(&mut account_info_raw).unwrap();

    let account: CliAccount = serde_json::from_str(&account_info_raw).unwrap();
    let UiAccountData::Binary(data, _) = account.keyed_account.account.data else {
        bail!("Expecting Binary format for fixtures")
    };
    let account = MarginfiAccount::try_deserialize(&mut STANDARD.decode(data)?.as_slice())?;

    assert_eq!(
        account.authority,
        pubkey!("7hmfVTuXc7HeX3YQjpiCXGVQuTeXonzjp795jorZukVR")
    );

    let balance_1 = account.lending_account.balances[0];
    assert!(!balance_1.active);
    assert_eq!(
        I80F48::from(balance_1.asset_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.liability_shares),
        I80F48::from_str("0").unwrap()
    );
    assert_eq!(
        I80F48::from(balance_1.emissions_outstanding),
        I80F48::from_str("0").unwrap()
    );

    Ok(())
}
