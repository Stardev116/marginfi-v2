// Runs once per user to init the central account that manages their kamino positions. Other users
// can run this to pay rent on behalf of a user.
use crate::constants::USER_ACCOUNT_SEED;
use crate::state::UserAccount;
use anchor_lang::prelude::*;

#[allow(unused_variables)]
pub fn init_user_account(ctx: Context<InitUser>) -> Result<()> {
    let mut user_account = ctx.accounts.user_account.load_init()?;

    user_account.key = ctx.accounts.user_account.key();
    user_account.user = ctx.accounts.user.key();
    user_account.bump_seed = ctx.bumps.user_account;

    user_account.last_activity = Clock::get().unwrap().unix_timestamp;

    Ok(())
}

#[derive(Accounts)]
pub struct InitUser<'info> {
    /// Pays the init fee
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: unchecked, anyone can pay to create a user account for anyone else.
    pub user: UncheckedAccount<'info>,

    #[account(
        init,
        seeds = [
            user.key().as_ref(),
            USER_ACCOUNT_SEED.as_bytes()
        ],
        bump,
        payer = payer,
        space = 8 + UserAccount::LEN,
    )]
    pub user_account: AccountLoader<'info, UserAccount>,

    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
}
