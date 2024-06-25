use account_state::{AccountInfoCache, AccountsState};
use anchor_lang::{
    accounts::{interface::Interface, interface_account::InterfaceAccount},
    prelude::{AccountInfo, AccountLoader, Context, Program, Pubkey, Rent, Signer, Sysvar},
    Discriminator, Key,
};
use arbitrary_helpers::{AccountIdx, AssetAmount, BankAndOracleConfig, BankIdx, PriceChange};
use bank_accounts::{get_bank_map, BankAccounts};

use fixed_macro::types::I80F48;

use marginfi::{
    instructions::LendingPoolAddBankBumps,
    prelude::MarginfiGroup,
    state::{
        marginfi_account::MarginfiAccount,
        marginfi_group::{Bank, BankConfig, BankVaultType, InterestRateConfig},
    },
};
use metrics::{MetricAction, Metrics};

use solana_program::system_program;

use std::{
    collections::{BTreeMap, HashMap},
    mem::size_of,
    ops::AddAssign,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use stubs::test_syscall_stubs;
use user_accounts::UserAccount;
use utils::{
    account_info_lifetime_shortener as ails, account_info_ref_lifetime_shortener as airls,
    account_info_slice_lifetime_shortener as aisls,
};

pub mod account_state;
pub mod arbitrary_helpers;
pub mod bank_accounts;
pub mod metrics;
pub mod stubs;
pub mod user_accounts;
pub mod utils;

type SplAccount = spl_token::state::Account;

pub struct MarginfiFuzzContext<'info> {
    pub marginfi_group: AccountInfo<'info>,
    pub banks: Vec<BankAccounts<'info>>,
    pub marginfi_accounts: Vec<UserAccount<'info>>,
    pub owner: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
    pub rent_sysvar: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub last_sysvar_current_timestamp: RwLock<u64>,
    pub metrics: Arc<RwLock<Metrics>>,
    pub state: &'info AccountsState,
}

impl<'state> MarginfiFuzzContext<'state> {
    pub fn setup(
        state: &'state AccountsState,
        bank_configs: &[BankAndOracleConfig],
        n_users: u8,
    ) -> Self {
        let system_program = state.new_program(system_program::id());
        let token_program = state.new_program(spl_token::id());
        let admin = state.new_sol_account(1_000_000);
        let rent_sysvar = state.new_rent_sysvar_account(Rent::free());
        let marginfi_group =
            initialize_marginfi_group(state, admin.clone(), system_program.clone());

        let mut marginfi_state = MarginfiFuzzContext {
            marginfi_group,
            banks: vec![],
            owner: admin,
            system_program,
            rent_sysvar,
            token_program,
            marginfi_accounts: vec![],
            last_sysvar_current_timestamp: RwLock::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            ),
            metrics: Arc::new(RwLock::new(Metrics::default())),
            state,
        };
        marginfi_state.advance_time(0);

        bank_configs
            .iter()
            .for_each(|config| marginfi_state.setup_bank(state, Rent::free(), config));

        let token_vec = marginfi_state.banks.iter().map(|b| *b.mint.key).collect();

        (0..n_users).into_iter().for_each(|_| {
            marginfi_state
                .create_marginfi_account(state, Rent::free(), &token_vec)
                .unwrap()
        });

        // Create an extra account for seeding the banks
        marginfi_state
            .create_marginfi_account(state, Rent::free(), &token_vec)
            .unwrap();

        // Seed the banks
        for bank_idx in 0..marginfi_state.banks.len() {
            marginfi_state
                .process_action_deposit(
                    &AccountIdx(marginfi_state.marginfi_accounts.len() as u8 - 1),
                    &BankIdx(bank_idx as u8),
                    &AssetAmount(
                        1_000
                            * 10_u64
                                .pow(marginfi_state.banks[bank_idx as usize].mint_decimals.into()),
                    ),
                )
                .unwrap();
        }

        marginfi_state
    }

    fn get_bank_map<'a>(&'a self) -> HashMap<Pubkey, &'a BankAccounts<'state>> {
        get_bank_map(&self.banks)
    }

    fn refresh_oracle_accounts(&self) {
        self.banks.iter().for_each(|bank| {
            bank.refresh_oracle(
                self.last_sysvar_current_timestamp
                    .read()
                    .unwrap()
                    .to_owned() as i64,
            )
            .unwrap()
        });
    }

    pub fn advance_time(&self, time: u64) {
        self.last_sysvar_current_timestamp
            .write()
            .unwrap()
            .add_assign(time);

        test_syscall_stubs(Some(
            *self.last_sysvar_current_timestamp.read().unwrap() as i64
        ));
    }

    pub fn setup_bank<'a>(
        &'a mut self,
        state: &'state AccountsState,
        rent: Rent,
        initial_bank_config: &BankAndOracleConfig,
    ) {
        log!("Setting up bank with config {:#?}", initial_bank_config);
        let bank = state.new_owned_account(size_of::<Bank>(), marginfi::id(), rent);

        let mint = state.new_token_mint(
            rent,
            initial_bank_config.mint_decimals,
            initial_bank_config.token_type,
        );
        let (liquidity_vault_authority, liquidity_vault_authority_bump) =
            state.new_vault_authority(BankVaultType::Liquidity, bank.key);
        let (liquidity_vault, liquidity_vault_bump) = state.new_vault_account(
            BankVaultType::Liquidity,
            mint.key,
            liquidity_vault_authority.key,
            bank.key,
        );

        let (insurance_vault_authority, insurance_vault_authority_bump) =
            state.new_vault_authority(BankVaultType::Insurance, bank.key);
        let (insurance_vault, insurance_vault_bump) = state.new_vault_account(
            BankVaultType::Insurance,
            mint.key,
            insurance_vault_authority.key,
            bank.key,
        );

        let (fee_vault_authority, fee_vault_authority_bump) =
            state.new_vault_authority(BankVaultType::Fee, bank.key);
        let (fee_vault, fee_vault_bump) = state.new_vault_account(
            BankVaultType::Fee,
            mint.key,
            fee_vault_authority.key,
            bank.key,
        );

        let oracle = state.new_oracle_account(
            rent,
            initial_bank_config.oracle_native_price as i64,
            *mint.key,
            initial_bank_config.mint_decimals as i32,
        );

        let add_bank_bumps = LendingPoolAddBankBumps {
            liquidity_vault_authority: liquidity_vault_authority_bump,
            liquidity_vault: liquidity_vault_bump,
            insurance_vault_authority: insurance_vault_authority_bump,
            insurance_vault: insurance_vault_bump,
            fee_vault_authority: fee_vault_authority_bump,
            fee_vault: fee_vault_bump,
        };

        {
            marginfi::instructions::marginfi_group::lending_pool_add_bank(
                Context::new(
                    &marginfi::ID,
                    &mut marginfi::instructions::LendingPoolAddBank {
                        marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))
                            .unwrap(),
                        admin: Signer::try_from(airls(&self.owner)).unwrap(),
                        fee_payer: Signer::try_from(airls(&self.owner)).unwrap(),
                        bank_mint: Box::new(InterfaceAccount::try_from(airls(&mint)).unwrap()),
                        bank: AccountLoader::try_from_unchecked(&marginfi::ID, airls(&bank))
                            .unwrap(),
                        liquidity_vault_authority: unsafe {
                            core::mem::transmute(liquidity_vault_authority.clone())
                        },
                        liquidity_vault: Box::new(
                            InterfaceAccount::try_from(airls(&liquidity_vault)).unwrap(),
                        ),
                        insurance_vault_authority: unsafe {
                            core::mem::transmute(insurance_vault_authority.clone())
                        },
                        insurance_vault: Box::new(
                            InterfaceAccount::try_from(airls(&insurance_vault)).unwrap(),
                        ),
                        fee_vault_authority: unsafe {
                            core::mem::transmute(fee_vault_authority.clone())
                        },
                        fee_vault: Box::new(InterfaceAccount::try_from(airls(&fee_vault)).unwrap()),
                        rent: Sysvar::from_account_info(airls(&self.rent_sysvar)).unwrap(),
                        token_program: Interface::try_from(airls(&self.token_program)).unwrap(),
                        system_program: Program::try_from(airls(&self.system_program)).unwrap(),
                    },
                    &[unsafe { core::mem::transmute(oracle.clone()) }],
                    add_bank_bumps,
                ),
                BankConfig {
                    asset_weight_init: initial_bank_config.asset_weight_init,
                    asset_weight_maint: initial_bank_config.asset_weight_maint,
                    liability_weight_init: initial_bank_config.liability_weight_init,
                    liability_weight_maint: initial_bank_config.liability_weight_maint,
                    deposit_limit: initial_bank_config.deposit_limit,
                    borrow_limit: initial_bank_config.borrow_limit,
                    interest_rate_config: InterestRateConfig {
                        optimal_utilization_rate: I80F48!(0.5).into(),
                        plateau_interest_rate: I80F48!(0.5).into(),
                        max_interest_rate: I80F48!(4).into(),
                        insurance_fee_fixed_apr: I80F48!(0.01).into(),
                        insurance_ir_fee: I80F48!(0.05).into(),
                        protocol_fixed_fee_apr: I80F48!(0.01).into(),
                        protocol_ir_fee: I80F48!(0.1).into(),
                        ..Default::default()
                    },
                    oracle_setup: marginfi::state::price::OracleSetup::PythEma,
                    oracle_keys: [
                        oracle.key(),
                        Pubkey::default(),
                        Pubkey::default(),
                        Pubkey::default(),
                        Pubkey::default(),
                    ],
                    operational_state:
                        marginfi::state::marginfi_group::BankOperationalState::Operational,
                    risk_tier: if !initial_bank_config.risk_tier_isolated {
                        marginfi::state::marginfi_group::RiskTier::Collateral
                    } else {
                        marginfi::state::marginfi_group::RiskTier::Isolated
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        }

        set_discriminator::<Bank>(bank.clone());

        self.banks.push(BankAccounts {
            bank,
            oracle,
            liquidity_vault,
            insurance_vault,
            fee_vault,
            mint,
            liquidity_vault_authority,
            insurance_vault_authority,
            fee_vault_authority,
            mint_decimals: initial_bank_config.mint_decimals,
        });
    }

    fn create_marginfi_account<'a>(
        &'a mut self,
        state: &'state AccountsState,
        rent: Rent,
        token_mints: &Vec<Pubkey>,
        // ) -> anyhow::Result<UserAccount<'a>> {
    ) -> anyhow::Result<()> {
        let marginfi_account =
            state.new_owned_account(size_of::<MarginfiAccount>(), marginfi::id(), rent);

        marginfi::instructions::marginfi_account::initialize_account(Context::new(
            &marginfi::id(),
            &mut marginfi::instructions::marginfi_account::MarginfiAccountInitialize {
                marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                marginfi_account: AccountLoader::try_from_unchecked(
                    &marginfi::ID,
                    airls(&marginfi_account),
                )?,
                authority: Signer::try_from(airls(&self.owner))?,
                fee_payer: Signer::try_from(airls(&self.owner))?,
                system_program: Program::try_from(airls(&self.system_program))?,
            },
            &[],
            Default::default(),
        ))?;

        let token_accounts = token_mints
            .iter()
            .map(|token| {
                state.new_token_account(token, self.owner.key, 100_000_000_000_000_000, rent)
            })
            .collect();

        set_discriminator::<MarginfiAccount>(marginfi_account.clone());

        self.marginfi_accounts
            .push(UserAccount::new(marginfi_account, token_accounts));

        Ok(())
    }

    pub fn process_action_deposit(
        &self,
        account_idx: &AccountIdx,
        bank_idx: &BankIdx,
        asset_amount: &AssetAmount,
    ) -> anyhow::Result<()> {
        let marginfi_account = &self.marginfi_accounts[account_idx.0 as usize];

        let bank = &self.banks[bank_idx.0 as usize];

        let cache = AccountInfoCache::new(&[
            marginfi_account.margin_account.clone(),
            bank.bank.clone(),
            marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
            bank.liquidity_vault.clone(),
        ]);

        let res = marginfi::instructions::marginfi_account::lending_account_deposit(
            Context::new(
                &marginfi::ID,
                &mut marginfi::instructions::LendingAccountDeposit {
                    marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                    marginfi_account: AccountLoader::try_from(airls(
                        &marginfi_account.margin_account,
                    ))?,
                    signer: Signer::try_from(airls(&self.owner))?,
                    bank: AccountLoader::try_from(airls(&bank.bank))?,
                    signer_token_account: ails(
                        marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
                    ),
                    bank_liquidity_vault: ails(bank.liquidity_vault.clone()),
                    token_program: Interface::try_from(airls(&self.token_program))?,
                },
                &[],
                Default::default(),
            ),
            asset_amount.0,
        );

        if res.is_err() {
            cache.revert();
        }

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Deposit, res.is_ok());

        Ok(())
    }

    pub fn process_action_repay(
        &self,
        account_idx: &AccountIdx,
        bank_idx: &BankIdx,
        asset_amount: &AssetAmount,
        repay_all: bool,
    ) -> anyhow::Result<()> {
        let marginfi_account = &self.marginfi_accounts[account_idx.0 as usize];
        let bank = &self.banks[bank_idx.0 as usize];

        let cache = AccountInfoCache::new(&[
            marginfi_account.margin_account.clone(),
            bank.bank.clone(),
            marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
            bank.liquidity_vault.clone(),
        ]);

        let res = marginfi::instructions::marginfi_account::lending_account_repay(
            Context::new(
                &marginfi::ID,
                &mut marginfi::instructions::LendingAccountRepay {
                    marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                    marginfi_account: AccountLoader::try_from(airls(
                        &marginfi_account.margin_account,
                    ))?,
                    signer: Signer::try_from(airls(&self.owner))?,
                    bank: AccountLoader::try_from(airls(&bank.bank))?,
                    signer_token_account: ails(
                        marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
                    ),
                    bank_liquidity_vault: ails(bank.liquidity_vault.clone()),
                    token_program: Interface::try_from(airls(&self.token_program))?,
                },
                &[],
                Default::default(),
            ),
            asset_amount.0,
            Some(repay_all),
        );

        if res.is_err() {
            cache.revert();
        }

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Repay, res.is_ok());

        Ok(())
    }

    pub fn process_action_withdraw(
        &'state self,
        account_idx: &AccountIdx,
        bank_idx: &BankIdx,
        asset_amount: &AssetAmount,
        withdraw_all: Option<bool>,
    ) -> anyhow::Result<()> {
        self.refresh_oracle_accounts();
        let marginfi_account = &self.marginfi_accounts[account_idx.0 as usize];

        let bank = &self.banks[bank_idx.0 as usize];

        let cache = AccountInfoCache::new(&[
            marginfi_account.margin_account.clone(),
            bank.bank.clone(),
            marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
            bank.liquidity_vault.clone(),
        ]);

        let remove_all_bank = if let Some(withdraw_all) = withdraw_all {
            if withdraw_all {
                vec![bank.bank.key()]
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let remaining_accounts =
            marginfi_account.get_remaining_accounts(&self.get_bank_map(), vec![], remove_all_bank);
        let res = marginfi::instructions::marginfi_account::lending_account_withdraw(
            Context::new(
                &marginfi::ID,
                &mut marginfi::instructions::LendingAccountWithdraw {
                    marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                    marginfi_account: AccountLoader::try_from(airls(
                        &marginfi_account.margin_account,
                    ))?,
                    signer: Signer::try_from(airls(&self.owner))?,
                    bank: AccountLoader::try_from(airls(&bank.bank))?,
                    token_program: Interface::try_from(airls(&self.token_program))?,
                    destination_token_account: InterfaceAccount::try_from(airls(
                        &marginfi_account.token_accounts[bank_idx.0 as usize],
                    ))?,
                    bank_liquidity_vault_authority: ails(bank.liquidity_vault_authority.clone()),
                    bank_liquidity_vault: InterfaceAccount::try_from(airls(&bank.liquidity_vault))?,
                },
                aisls(&remaining_accounts),
                Default::default(),
            ),
            asset_amount.0,
            withdraw_all,
        );

        if res.is_err() {
            cache.revert();
        }

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Withdraw, res.is_ok());

        Ok(())
    }

    pub fn process_action_borrow(
        &'state self,
        account_idx: &AccountIdx,
        bank_idx: &BankIdx,
        asset_amount: &AssetAmount,
    ) -> anyhow::Result<()> {
        self.refresh_oracle_accounts();

        let marginfi_account = &self.marginfi_accounts[account_idx.0 as usize];
        let bank = &self.banks[bank_idx.0 as usize];
        let cache = AccountInfoCache::new(&[
            marginfi_account.margin_account.clone(),
            bank.bank.clone(),
            marginfi_account.token_accounts[bank_idx.0 as usize].clone(),
            bank.liquidity_vault.clone(),
        ]);

        let remaining_accounts = marginfi_account.get_remaining_accounts(
            &self.get_bank_map(),
            vec![bank.bank.key()],
            vec![],
        );
        let res = marginfi::instructions::marginfi_account::lending_account_borrow(
            Context::new(
                &marginfi::ID,
                &mut marginfi::instructions::LendingAccountBorrow {
                    marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                    marginfi_account: AccountLoader::try_from(airls(
                        &marginfi_account.margin_account,
                    ))?,
                    signer: Signer::try_from(airls(&self.owner))?,
                    bank: AccountLoader::try_from(airls(&bank.bank))?,
                    token_program: Interface::try_from(airls(&self.token_program))?,
                    destination_token_account: InterfaceAccount::try_from(airls(
                        &marginfi_account.token_accounts[bank_idx.0 as usize],
                    ))?,
                    bank_liquidity_vault_authority: ails(bank.liquidity_vault_authority.clone()),
                    bank_liquidity_vault: InterfaceAccount::try_from(airls(&bank.liquidity_vault))?,
                },
                aisls(&remaining_accounts),
                Default::default(),
            ),
            asset_amount.0,
        );

        let is_ok = res.is_ok();

        if !is_ok {
            log!("{}", res.unwrap_err());
            cache.revert();
        }

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Borrow, is_ok);

        Ok(())
    }

    pub fn process_liquidate_account(
        &'state self,
        liquidator_idx: &AccountIdx,
        liquidatee_idx: &AccountIdx,
        asset_amount: &AssetAmount,
    ) -> anyhow::Result<()> {
        self.refresh_oracle_accounts();
        let liquidator_account = &self.marginfi_accounts[liquidator_idx.0 as usize];
        let liquidatee_account = &self.marginfi_accounts[liquidatee_idx.0 as usize];

        let (asset_bank_idx, liab_bank_idx) =
            if let Some(a) = liquidatee_account.get_liquidation_banks(&self.banks) {
                a
            } else {
                self.metrics
                    .write()
                    .unwrap()
                    .update_metric(MetricAction::Liquidate, false);

                return Ok(());
            };

        let asset_bank = &self.banks[asset_bank_idx.0 as usize];
        let liab_bank = &self.banks[liab_bank_idx.0 as usize];

        let account_cache = AccountInfoCache::new(&[
            liquidator_account.margin_account.clone(),
            liquidatee_account.margin_account.clone(),
            asset_bank.bank.clone(),
            asset_bank.liquidity_vault.clone(),
            liab_bank.bank.clone(),
            liab_bank.liquidity_vault.clone(),
            liab_bank.insurance_vault.clone(),
        ]);

        let mut remaining_accounts = vec![asset_bank.oracle.clone(), liab_bank.oracle.clone()];

        let mut liquidator_remaining_accounts = liquidator_account.get_remaining_accounts(
            &self.get_bank_map(),
            vec![asset_bank.bank.key(), liab_bank.bank.key()],
            vec![],
        );
        let mut liquidatee_remaining_accounts =
            liquidatee_account.get_remaining_accounts(&self.get_bank_map(), vec![], vec![]);

        remaining_accounts.append(&mut liquidator_remaining_accounts);
        remaining_accounts.append(&mut liquidatee_remaining_accounts);

        let res = marginfi::instructions::lending_account_liquidate(
            Context::new(
                &marginfi::id(),
                &mut marginfi::instructions::LendingAccountLiquidate {
                    marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                    asset_bank: AccountLoader::try_from(airls(&asset_bank.bank))?,
                    liab_bank: AccountLoader::try_from(airls(&liab_bank.bank))?,
                    liquidator_marginfi_account: AccountLoader::try_from(airls(
                        &liquidator_account.margin_account,
                    ))?,
                    signer: Signer::try_from(airls(&self.owner))?,
                    liquidatee_marginfi_account: AccountLoader::try_from(airls(
                        &liquidatee_account.margin_account,
                    ))?,
                    bank_liquidity_vault_authority: ails(
                        liab_bank.liquidity_vault_authority.clone(),
                    ),
                    bank_liquidity_vault: Box::new(InterfaceAccount::try_from(airls(
                        &liab_bank.liquidity_vault,
                    ))?),
                    bank_insurance_vault: ails(liab_bank.insurance_vault.clone()),
                    token_program: Interface::try_from(airls(&self.token_program))?,
                },
                aisls(&remaining_accounts),
                Default::default(),
            ),
            asset_amount.0,
        );

        let is_ok = res.is_ok();

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Liquidate, is_ok);

        if !is_ok {
            account_cache.revert();
            log!("Error Liquidate {:?}", res.unwrap_err());
        } else {
            self.process_handle_bankruptcy(liquidatee_idx, &liab_bank_idx)?;
        }

        Ok(())
    }

    pub fn process_handle_bankruptcy(
        &'state self,
        account_idx: &AccountIdx,
        bank_idx: &BankIdx,
    ) -> anyhow::Result<()> {
        log!("Action: Handle Bankruptcy");

        let marginfi_account = &self.marginfi_accounts[account_idx.0 as usize];
        let bank = &self.banks[bank_idx.0 as usize];

        let cache = AccountInfoCache::new(&[
            bank.bank.clone(),
            marginfi_account.margin_account.clone(),
            bank.liquidity_vault.clone(),
            bank.insurance_vault.clone(),
        ]);

        let remaining_accounts =
            marginfi_account.get_remaining_accounts(&self.get_bank_map(), vec![], vec![]);
        let res = marginfi::instructions::lending_pool_handle_bankruptcy(Context::new(
            &marginfi::ID,
            &mut marginfi::instructions::LendingPoolHandleBankruptcy {
                marginfi_group: AccountLoader::try_from(airls(&self.marginfi_group))?,
                signer: Signer::try_from(airls(&self.owner))?,
                bank: AccountLoader::try_from(airls(&bank.bank))?,
                marginfi_account: AccountLoader::try_from(airls(&marginfi_account.margin_account))?,
                liquidity_vault: ails(bank.liquidity_vault.clone()),
                insurance_vault: Box::new(InterfaceAccount::try_from(airls(
                    &bank.insurance_vault,
                ))?),
                insurance_vault_authority: ails(bank.insurance_vault_authority.clone()),
                token_program: Interface::try_from(airls(&self.token_program))?,
            },
            aisls(&remaining_accounts),
            Default::default(),
        ));

        if res.is_err() {
            cache.revert();
        }

        self.metrics
            .write()
            .unwrap()
            .update_metric(MetricAction::Bankruptcy, res.is_ok());

        Ok(())
    }

    pub fn process_update_oracle(
        &self,
        bank_idx: &BankIdx,
        price_change: &PriceChange,
    ) -> anyhow::Result<()> {
        log!("Action: Update Oracle");
        let bank = &self.banks[bank_idx.0 as usize];

        bank.update_oracle(price_change.0)?;

        self.metrics.write().unwrap().price_update += 1;

        Ok(())
    }
}

pub fn set_discriminator<T: Discriminator>(ai: AccountInfo) {
    let mut data = ai.try_borrow_mut_data().unwrap();

    if data[..8].ne(&[0u8; 8]) {
        panic!("Account discriminator is already set");
    }

    data[..8].copy_from_slice(&T::DISCRIMINATOR);
}

fn initialize_marginfi_group<'a>(
    state: &'a AccountsState,
    admin: AccountInfo<'a>,
    system_program: AccountInfo<'a>,
) -> AccountInfo<'a> {
    let program_id = marginfi::id();
    let marginfi_group =
        state.new_owned_account(size_of::<MarginfiGroup>(), program_id, Rent::free());

    marginfi::instructions::marginfi_group::initialize_group(Context::new(
        &marginfi::id(),
        &mut marginfi::instructions::MarginfiGroupInitialize {
            // Unchecked because we are initializing the account.
            marginfi_group: AccountLoader::try_from_unchecked(&program_id, airls(&marginfi_group))
                .unwrap(),
            admin: Signer::try_from(airls(&admin)).unwrap(),
            system_program: Program::try_from(airls(&system_program)).unwrap(),
        },
        &[],
        Default::default(),
    ))
    .unwrap();

    set_discriminator::<MarginfiGroup>(marginfi_group.clone());

    marginfi_group
}

#[cfg(test)]
mod tests {
    use fixed::types::I80F48;
    use marginfi::state::marginfi_account::RiskEngine;
    use pyth_sdk_solana::state::SolanaPriceAccount;

    use super::*;
    #[test]
    fn deposit_test() {
        let account_state = AccountsState::new();

        let a = MarginfiFuzzContext::setup(&account_state, &[BankAndOracleConfig::dummy(); 2], 2);

        let al =
            AccountLoader::<MarginfiGroup>::try_from_unchecked(&marginfi::id(), &a.marginfi_group)
                .unwrap();

        assert_eq!(al.load().unwrap().admin, a.owner.key());

        a.process_action_deposit(&AccountIdx(0), &BankIdx(0), &AssetAmount(1000))
            .unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();
        let marginfi_account = marginfi_account_ai.load().unwrap();

        assert_eq!(
            I80F48::from(marginfi_account.lending_account.balances[0].asset_shares),
            I80F48!(1000)
        );
    }

    #[test]
    fn borrow_test() {
        let account_state = AccountsState::new();
        let a = MarginfiFuzzContext::setup(&account_state, &[BankAndOracleConfig::dummy(); 2], 2);

        a.process_action_deposit(&AccountIdx(1), &BankIdx(1), &AssetAmount(1000))
            .unwrap();
        a.process_action_deposit(&AccountIdx(0), &BankIdx(0), &AssetAmount(1000))
            .unwrap();
        a.process_action_borrow(&AccountIdx(0), &BankIdx(1), &AssetAmount(100))
            .unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();

        {
            let marginfi_account = marginfi_account_ai.load().unwrap();

            assert_eq!(
                I80F48::from(marginfi_account.lending_account.balances[0].asset_shares),
                I80F48!(1000)
            );
            assert_eq!(
                I80F48::from(marginfi_account.lending_account.balances[1].liability_shares),
                I80F48!(100)
            );
        }

        a.process_action_repay(&AccountIdx(0), &BankIdx(1), &AssetAmount(100), false)
            .unwrap();

        let marginfi_account = marginfi_account_ai.load().unwrap();

        assert_eq!(
            I80F48::from(marginfi_account.lending_account.balances[1].liability_shares),
            I80F48!(0)
        );
    }

    #[test]
    fn liquidation_test() {
        let account_state = AccountsState::new();
        let a = MarginfiFuzzContext::setup(&account_state, &[BankAndOracleConfig::dummy(); 2], 3);

        a.process_action_deposit(&AccountIdx(1), &BankIdx(1), &AssetAmount(1000))
            .unwrap();
        a.process_action_deposit(&AccountIdx(0), &BankIdx(0), &AssetAmount(1000))
            .unwrap();
        a.process_action_borrow(&AccountIdx(0), &BankIdx(1), &AssetAmount(500))
            .unwrap();

        a.banks[1].log_oracle_price().unwrap();

        a.process_update_oracle(&BankIdx(1), &PriceChange(10000000000000))
            .unwrap();

        a.banks[1].log_oracle_price().unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();

        {
            let marginfi_account = marginfi_account_ai.load().unwrap();
            let margin_account = &a.marginfi_accounts[0];
            let bank_map = a.get_bank_map();
            let remaining_accounts =
                margin_account.get_remaining_accounts(&bank_map, vec![], vec![]);

            let re = RiskEngine::new(&marginfi_account, aisls(&remaining_accounts)).unwrap();

            let health = re
                .get_account_health(
                    marginfi::state::marginfi_account::RiskRequirementType::Maintenance,
                )
                .unwrap();

            println!("Health {health}");
        }

        a.process_action_deposit(&AccountIdx(2), &BankIdx(1), &AssetAmount(1000))
            .unwrap();

        a.process_liquidate_account(&AccountIdx(2), &AccountIdx(0), &AssetAmount(50))
            .unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();

        let marginfi_account = marginfi_account_ai.load().unwrap();

        assert_eq!(
            I80F48::from(marginfi_account.lending_account.balances[0].asset_shares),
            I80F48!(950)
        );
    }

    #[test]
    fn liquidation_and_bankruptcy() {
        let account_state = AccountsState::new();

        let a = MarginfiFuzzContext::setup(&account_state, &[BankAndOracleConfig::dummy(); 2], 3);

        a.process_action_deposit(&AccountIdx(1), &BankIdx(1), &AssetAmount(1000))
            .unwrap();
        a.process_action_deposit(&AccountIdx(0), &BankIdx(0), &AssetAmount(1000))
            .unwrap();
        a.process_action_borrow(&AccountIdx(0), &BankIdx(1), &AssetAmount(500))
            .unwrap();

        a.process_update_oracle(&BankIdx(1), &PriceChange(1000000000000))
            .unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();

        {
            let marginfi_account = marginfi_account_ai.load().unwrap();
            let margin_account = &a.marginfi_accounts[0];
            let bank_map = a.get_bank_map();
            let remaining_accounts =
                margin_account.get_remaining_accounts(&bank_map, vec![], vec![]);

            let re = RiskEngine::new(&marginfi_account, aisls(&remaining_accounts)).unwrap();

            let health = re
                .get_account_health(
                    marginfi::state::marginfi_account::RiskRequirementType::Maintenance,
                )
                .unwrap();

            println!("Health {health}");
        }

        a.process_action_deposit(&AccountIdx(2), &BankIdx(1), &AssetAmount(1000))
            .unwrap();

        a.process_liquidate_account(&AccountIdx(2), &AccountIdx(0), &AssetAmount(1000))
            .unwrap();

        let marginfi_account_ai = AccountLoader::<MarginfiAccount>::try_from_unchecked(
            &marginfi::id(),
            &a.marginfi_accounts[0].margin_account,
        )
        .unwrap();

        let marginfi_account = marginfi_account_ai.load().unwrap();

        assert_eq!(
            I80F48::from(marginfi_account.lending_account.balances[0].asset_shares),
            I80F48!(0)
        );
        assert_eq!(
            I80F48::from(marginfi_account.lending_account.balances[0].liability_shares),
            I80F48!(0)
        );
    }

    #[test]
    fn price_update() {
        let account_state = AccountsState::new();

        let a = MarginfiFuzzContext::setup(&account_state, &[BankAndOracleConfig::dummy(); 2], 3);

        let price = {
            let data = a.banks[0].oracle.try_borrow_data().unwrap();
            let data = bytemuck::from_bytes::<SolanaPriceAccount>(&data);

            data.ema_price.val
        };

        a.process_update_oracle(&BankIdx(0), &PriceChange(1100))
            .unwrap();

        let new_price = {
            let data = a.banks[0].oracle.try_borrow_data().unwrap();
            let data = bytemuck::from_bytes::<SolanaPriceAccount>(&data);
            data.ema_price.val
        };

        assert_eq!(price, new_price - 1100);
    }
}
