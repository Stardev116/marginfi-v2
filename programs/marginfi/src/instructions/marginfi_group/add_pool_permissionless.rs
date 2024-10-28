// Adds a ASSET_TAG_STAKED type bank to a group with sane defaults. Used by validators to add their
// freshly-minted LST to a group so users can borrow SOL against it

// TODO should we support this for riskTier::Isolated too?

// TODO pick a hardcoded oracle

// TODO pick a hardcoded interest regmine

// TODO pick a hardcoded asset weight (~85%?) and `total_asset_value_init_limit`

// TODO pick a hardcoded max oracle age (~30s?)

// TODO pick a hardcoded initial deposit limit () //

// TODO should the group admin need to opt in to this functionality (configure the group)? We could
// also configure the key that assumes default admin here instead of using the group's admin
use crate::{
    check,
    constants::{
        ASSET_TAG_STAKED, FEE_VAULT_AUTHORITY_SEED, FEE_VAULT_SEED, INSURANCE_VAULT_AUTHORITY_SEED,
        INSURANCE_VAULT_SEED, LIQUIDITY_VAULT_AUTHORITY_SEED, LIQUIDITY_VAULT_SEED,
        NATIVE_STAKE_ID, SPL_SINGLE_POOL_ID, STAKED_SETTINGS_SEED,
    },
    events::{GroupEventHeader, LendingPoolBankCreateEvent},
    state::{
        marginfi_group::{
            Bank, BankConfigCompact, BankOperationalState, InterestRateConfig, MarginfiGroup,
        },
        price::OracleSetup,
        staked_settings::StakedSettings,
    },
    MarginfiError, MarginfiResult,
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::*;
use fixed_macro::types::I80F48;

pub fn lending_pool_add_bank_permissionless(
    ctx: Context<LendingPoolAddBankPermissionless>,
    _bank_seed: u64,
) -> MarginfiResult {
    let LendingPoolAddBankPermissionless {
        bank_mint,
        liquidity_vault,
        insurance_vault,
        fee_vault,
        bank: bank_loader,
        ..
    } = ctx.accounts;

    let mut bank = bank_loader.load_init()?;
    let settings = ctx.accounts.staked_settings.load()?;
    let group = ctx.accounts.marginfi_group.load()?;

    let liquidity_vault_bump = ctx.bumps.liquidity_vault;
    let liquidity_vault_authority_bump = ctx.bumps.liquidity_vault_authority;
    let insurance_vault_bump = ctx.bumps.insurance_vault;
    let insurance_vault_authority_bump = ctx.bumps.insurance_vault_authority;
    let fee_vault_bump = ctx.bumps.fee_vault;
    let fee_vault_authority_bump = ctx.bumps.fee_vault_authority;

    // These are placeholder values: staked collateral positions do not support borrowing.
    let default_ir_config = InterestRateConfig {
        ..Default::default()
    };

    let default_config: BankConfigCompact = BankConfigCompact {
        asset_weight_init: settings.asset_weight_init,
        asset_weight_maint: settings.asset_weight_maint,
        liability_weight_init: I80F48!(1.5).into(), // placeholder
        liability_weight_maint: I80F48!(1.25).into(), // placeholder
        deposit_limit: settings.deposit_limit,
        interest_rate_config: default_ir_config.into(), // placeholder
        operational_state: BankOperationalState::Operational,
        oracle_setup: OracleSetup::StakedWithPythPush,
        oracle_key: settings.oracle,
        borrow_limit: 0,
        risk_tier: settings.risk_tier,
        asset_tag: ASSET_TAG_STAKED,
        _pad0: [0; 6],
        total_asset_value_init_limit: settings.total_asset_value_init_limit,
        oracle_max_age: settings.oracle_max_age,
    };

    *bank = Bank::new(
        ctx.accounts.marginfi_group.key(),
        default_config.into(),
        bank_mint.key(),
        bank_mint.decimals,
        liquidity_vault.key(),
        insurance_vault.key(),
        fee_vault.key(),
        Clock::get().unwrap().unix_timestamp,
        liquidity_vault_bump,
        liquidity_vault_authority_bump,
        insurance_vault_bump,
        insurance_vault_authority_bump,
        fee_vault_bump,
        fee_vault_authority_bump,
    );

    {
        let program_id = &SPL_SINGLE_POOL_ID;
        let mint_actual = bank_mint.key();
        let stake_pool_bytes = &ctx.accounts.stake_pool.key().to_bytes();
        // Validate the given stake_pool derives the same lst_mint, proving stake_pool is correct
        let (exp_mint, _) = Pubkey::find_program_address(&[b"mint", stake_pool_bytes], program_id);
        check!(
            exp_mint == mint_actual,
            MarginfiError::StakePoolValidationFailed
        );
        // Validate the now-proven stake_pool derives the given sol_pool
        let (exp_pool, _) = Pubkey::find_program_address(&[b"stake", stake_pool_bytes], program_id);
        check!(
            exp_pool == ctx.accounts.sol_pool.key(),
            MarginfiError::StakePoolValidationFailed
        );
        // Sanity check these accounts exist and have the correct owning program
        check!(
            ctx.accounts.stake_pool.owner == &NATIVE_STAKE_ID,
            MarginfiError::StakePoolValidationFailed
        );
        check!(
            ctx.accounts.sol_pool.owner == program_id,
            MarginfiError::StakePoolValidationFailed
        );

        bank.config.oracle_keys[1] = mint_actual.key();
        bank.config.oracle_keys[2] = ctx.accounts.stake_pool.key();
    }

    bank.config.validate()?;
    bank.config.validate_oracle_setup(ctx.remaining_accounts)?;

    emit!(LendingPoolBankCreateEvent {
        header: GroupEventHeader {
            marginfi_group: ctx.accounts.marginfi_group.key(),
            signer: Some(group.admin)
        },
        bank: bank_loader.key(),
        mint: bank_mint.key(),
    });

    Ok(())
}

#[derive(Accounts)]
#[instruction(bank_seed: u64)]
pub struct LendingPoolAddBankPermissionless<'info> {
    pub marginfi_group: AccountLoader<'info, MarginfiGroup>,

    #[account(
        has_one = marginfi_group
    )]
    pub staked_settings: AccountLoader<'info, StakedSettings>,

    #[account(mut)]
    pub fee_payer: Signer<'info>,

    /// Mint of the spl-single-pool LST (a PDA derived from `stake_pool`)
    ///
    /// TODO test the below assumption
    /// CHECK: passing a mint here that is not actually a staked collateral LST is not possible
    /// because the sol_pool and stake_pool will not derive to a valid PDA which is also owned by
    /// the staking program and spl-single-pool program.
    pub bank_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: Validated using `stake_pool`
    pub sol_pool: AccountInfo<'info>,

    /// CHECK: We validate this is correct backwards, by deriving the PDA of the `bank_mint` using
    /// this key.
    ///
    /// If derives the same `bank_mint`, then this must be the correct stake pool for that mint, and
    /// we can subsequently use it to validate the `sol_pool`
    pub stake_pool: AccountInfo<'info>,

    #[account(
        init,
        space = 8 + std::mem::size_of::<Bank>(),
        payer = fee_payer,
        seeds = [
            marginfi_group.key().as_ref(),
            bank_mint.key().as_ref(),
            &bank_seed.to_le_bytes(),
        ],
        bump,
    )]
    pub bank: AccountLoader<'info, Bank>,

    /// CHECK: ⋐ ͡⋄ ω ͡⋄ ⋑
    #[account(
        seeds = [
            LIQUIDITY_VAULT_AUTHORITY_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump
    )]
    pub liquidity_vault_authority: AccountInfo<'info>,

    #[account(
        init,
        payer = fee_payer,
        token::mint = bank_mint,
        token::authority = liquidity_vault_authority,
        seeds = [
            LIQUIDITY_VAULT_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump,
    )]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: ⋐ ͡⋄ ω ͡⋄ ⋑
    #[account(
        seeds = [
            INSURANCE_VAULT_AUTHORITY_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump
    )]
    pub insurance_vault_authority: AccountInfo<'info>,

    #[account(
        init,
        payer = fee_payer,
        token::mint = bank_mint,
        token::authority = insurance_vault_authority,
        seeds = [
            INSURANCE_VAULT_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump,
    )]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: ⋐ ͡⋄ ω ͡⋄ ⋑
    #[account(
        seeds = [
            FEE_VAULT_AUTHORITY_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump
    )]
    pub fee_vault_authority: AccountInfo<'info>,

    #[account(
        init,
        payer = fee_payer,
        token::mint = bank_mint,
        token::authority = fee_vault_authority,
        seeds = [
            FEE_VAULT_SEED.as_bytes(),
            bank.key().as_ref(),
        ],
        bump,
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub rent: Sysvar<'info, Rent>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}
