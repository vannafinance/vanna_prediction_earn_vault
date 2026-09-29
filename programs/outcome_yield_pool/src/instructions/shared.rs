use crate::constants::*;
use crate::errors::PoolError;
use crate::math::checked_add;
use crate::predict::SetAccounts;
use crate::state::Pool;
use crate::token;
use crate::vanna::{self, Vanna};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::Token;

/// Signer seeds for a pool PDA. The caller keeps `bump` alive for as long as the seeds are used.
pub fn pool_seeds<'a>(pool: &'a Pool, bump: &'a [u8; 1]) -> [&'a [u8]; 4] {
    [POOL_SEED, pool.market.as_ref(), pool.vanna_reserve.as_ref(), bump]
}

/// The prediCt accounts `split` and `merge` need, plus the pool's own CASH/YES/NO accounts.
#[derive(Accounts)]
pub struct PredictAccounts<'info> {
    /// CHECK: key checked against `pool.market`; prediCt validates the rest.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.collateral_mint`.
    pub collateral_mint: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.yes_mint`.
    #[account(mut)]
    pub yes_mint: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.no_mint`.
    #[account(mut)]
    pub no_mint: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.market_cash_vault`.
    #[account(mut)]
    pub market_cash_vault: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.pool_cash`.
    #[account(mut)]
    pub pool_cash: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.pool_yes`.
    #[account(mut)]
    pub pool_yes: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.pool_no`.
    #[account(mut)]
    pub pool_no: UncheckedAccount<'info>,
    /// CHECK: key checked against `PREDICT_PROGRAM_ID`.
    pub predict_program: UncheckedAccount<'info>,
}

impl<'info> PredictAccounts<'info> {
    pub fn validate(&self, pool: &Pool) -> Result<()> {
        require_keys_eq!(self.market.key(), pool.market, PoolError::NotPredictMarket);
        require_keys_eq!(self.collateral_mint.key(), pool.collateral_mint, PoolError::InvalidCollateral);
        require_keys_eq!(self.yes_mint.key(), pool.yes_mint, PoolError::OutcomeMintMismatch);
        require_keys_eq!(self.no_mint.key(), pool.no_mint, PoolError::OutcomeMintMismatch);
        require_keys_eq!(self.market_cash_vault.key(), pool.market_cash_vault, PoolError::NotPredictMarket);
        require_keys_eq!(self.pool_cash.key(), pool.pool_cash, PoolError::OutcomeMintMismatch);
        require_keys_eq!(self.pool_yes.key(), pool.pool_yes, PoolError::OutcomeMintMismatch);
        require_keys_eq!(self.pool_no.key(), pool.pool_no, PoolError::OutcomeMintMismatch);
        require_keys_eq!(self.predict_program.key(), PREDICT_PROGRAM_ID, PoolError::NotPredictMarket);
        Ok(())
    }

    pub fn pool_outcome(&self, is_yes: bool) -> &UncheckedAccount<'info> {
        if is_yes {
            &self.pool_yes
        } else {
            &self.pool_no
        }
    }

    pub fn outcome_mint(&self, is_yes: bool) -> &UncheckedAccount<'info> {
        if is_yes {
            &self.yes_mint
        } else {
            &self.no_mint
        }
    }

    pub fn set_accounts<'a>(
        &'a self,
        authority: &'a AccountInfo<'info>,
        token_program: &'a AccountInfo<'info>,
    ) -> SetAccounts<'a, 'info> {
        SetAccounts {
            authority,
            market: self.market.as_ref(),
            collateral_mint: self.collateral_mint.as_ref(),
            yes_mint: self.yes_mint.as_ref(),
            no_mint: self.no_mint.as_ref(),
            authority_cash: self.pool_cash.as_ref(),
            market_cash_vault: self.market_cash_vault.as_ref(),
            authority_yes: self.pool_yes.as_ref(),
            authority_no: self.pool_no.as_ref(),
            token_program,
            predict_program: self.predict_program.as_ref(),
        }
    }
}

/// Vanna's USDC reserve and the pool's position in it: idle USDC plus reserve shares.
#[derive(Accounts)]
pub struct VannaAccounts<'info> {
    /// CHECK: key checked against `VANNA_PROGRAM_ID`.
    pub vanna_program: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.vanna_protocol_config`.
    pub vanna_protocol_config: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.vanna_asset_config`.
    pub vanna_asset_config: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.vanna_reserve`; writable because every read refreshes it first.
    #[account(mut)]
    pub vanna_reserve: UncheckedAccount<'info>,
    /// CHECK: key checked against `USDC_MINT`.
    pub usdc_mint: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.vanna_liquidity_vault`.
    #[account(mut)]
    pub vanna_liquidity_vault: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.vanna_share_mint`.
    #[account(mut)]
    pub vanna_share_mint: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.pool_usdc`.
    #[account(mut)]
    pub pool_usdc: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.pool_vanna_shares`.
    #[account(mut)]
    pub pool_vanna_shares: UncheckedAccount<'info>,
    pub spl_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

impl<'info> VannaAccounts<'info> {
    pub fn validate(&self, pool: &Pool) -> Result<()> {
        require_keys_eq!(self.vanna_program.key(), VANNA_PROGRAM_ID, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.vanna_protocol_config.key(), pool.vanna_protocol_config, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.vanna_asset_config.key(), pool.vanna_asset_config, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.vanna_reserve.key(), pool.vanna_reserve, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.usdc_mint.key(), USDC_MINT, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.vanna_liquidity_vault.key(), pool.vanna_liquidity_vault, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.vanna_share_mint.key(), pool.vanna_share_mint, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.pool_usdc.key(), pool.pool_usdc, PoolError::VannaAccountMismatch);
        require_keys_eq!(self.pool_vanna_shares.key(), pool.pool_vanna_shares, PoolError::VannaAccountMismatch);
        Ok(())
    }

    pub fn vanna<'a>(&'a self, pool: &'a AccountInfo<'info>) -> Vanna<'a, 'info> {
        Vanna {
            program: self.vanna_program.as_ref(),
            protocol_config: self.vanna_protocol_config.as_ref(),
            asset_config: self.vanna_asset_config.as_ref(),
            reserve: self.vanna_reserve.as_ref(),
            usdc_mint: self.usdc_mint.as_ref(),
            liquidity_vault: self.vanna_liquidity_vault.as_ref(),
            share_mint: self.vanna_share_mint.as_ref(),
            lender: pool,
            lender_usdc: self.pool_usdc.as_ref(),
            lender_shares: self.pool_vanna_shares.as_ref(),
            token_program: self.spl_token_program.as_ref(),
            associated_token_program: self.associated_token_program.as_ref(),
            system_program: self.system_program.as_ref(),
        }
    }

    /// USDC the pool's reserve shares would redeem for now. Short-circuits on an empty position, so a pool that holds
    /// no shares never calls into Vanna and cannot be blocked by it.
    pub fn position_value(&self, pool: &AccountInfo<'info>) -> Result<u64> {
        let shares = token::amount(self.pool_vanna_shares.as_ref())?;
        if shares == 0 {
            return Ok(0);
        }
        self.vanna(pool).refresh()?;
        let reserve = vanna::read_reserve(self.vanna_reserve.as_ref())?;
        vanna::shares_to_assets_down(shares, token::supply(self.vanna_share_mint.as_ref())?, reserve.lender_total_assets)
    }

    /// The pool's invested balance in CASH units: idle USDC plus the reserve position, each USDC counted at par. Every
    /// swap that converts between the two is checked against the same par, so no swap can make this overstate what
    /// the pool can pay.
    pub fn invested(&self, pool: &AccountInfo<'info>) -> Result<u64> {
        checked_add(token::amount(self.pool_usdc.as_ref())?, self.position_value(pool)?)
    }

    /// Makes at least `amount` USDC idle in the pool, redeeming the fewest reserve shares that cover the gap. Assumes
    /// `position_value` already refreshed the reserve in this instruction.
    pub fn ensure_idle_usdc(&self, pool: &AccountInfo<'info>, amount: u64, signer: &[&[&[u8]]]) -> Result<()> {
        let idle = token::amount(self.pool_usdc.as_ref())?;
        if idle >= amount {
            return Ok(());
        }
        let gap = amount - idle;
        let reserve = vanna::read_reserve(self.vanna_reserve.as_ref())?;
        let shares =
            vanna::assets_to_shares_up(gap, token::supply(self.vanna_share_mint.as_ref())?, reserve.lender_total_assets)?;
        self.vanna(pool).redeem(shares, gap, signer)
    }
}
