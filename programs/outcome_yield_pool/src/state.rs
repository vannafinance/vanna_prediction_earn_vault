use anchor_lang::prelude::*;

/// Program-wide configuration. The admin only curates which World markets may get a pool and the swap policy; it has
/// no control over any pool's funds. The resolver can settle a pool only after World has closed the pool's market
/// account.
#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    pub resolver: Pubkey,
    /// The only caller whose CASH -> USDC swaps may return less than par, by at most `max_swap_loss_bps`. Every
    /// other swap must be at par or better, so no caller can route pool funds through a venue it prices.
    pub keeper: Pubkey,
    pub max_swap_loss_bps: u16,
    pub bump: u8,
}

impl Config {
    /// Basis points of par a CASH -> USDC swap signed by `caller` may lose.
    pub fn swap_loss_bps(&self, caller: &Pubkey) -> u16 {
        if *caller == self.keeper {
            self.max_swap_loss_bps
        } else {
            0
        }
    }
}

/// Marks a World market as eligible for pools. Closing it stops new pools; existing pools keep working.
#[account]
#[derive(InitSpace)]
pub struct AllowedMarket {
    pub market: Pubkey,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum PoolStatus {
    /// The market is live: deposits, merges and par redemptions via `split` are available.
    Open,
    /// The market resolved. Split and merge are gone, so shares redeem against the snapshot winner.
    Settled { yes_won: bool },
}

/// One pool per (World market, Vanna reserve). Holds the market's funds in associated token accounts it owns, so
/// markets are isolated physically: a pool can only ever move tokens out of its own accounts, and its stake in the
/// shared Vanna reserve is its own share balance.
#[account]
#[derive(InitSpace)]
pub struct Pool {
    /// The prediCt Market account.
    pub market: Pubkey,
    /// Vanna's USDC reserve and the accounts `lender_supply` / `lender_redeem` need, read once at creation.
    pub vanna_reserve: Pubkey,
    pub vanna_protocol_config: Pubkey,
    pub vanna_asset_config: Pubkey,
    pub vanna_liquidity_vault: Pubkey,
    pub vanna_share_mint: Pubkey,
    /// Read from the prediCt Market once at creation.
    pub collateral_mint: Pubkey,
    pub yes_mint: Pubkey,
    pub no_mint: Pubkey,
    pub market_cash_vault: Pubkey,
    /// This pool's share mints, one per side. Mint authority is the pool.
    pub yes_share_mint: Pubkey,
    pub no_share_mint: Pubkey,
    /// The pool's own associated token accounts, recorded so every instruction can check them by key.
    pub pool_cash: Pubkey,
    pub pool_yes: Pubkey,
    pub pool_no: Pubkey,
    pub pool_usdc: Pubkey,
    pub pool_vanna_shares: Pubkey,
    pub status: PoolStatus,
    pub bump: u8,
}

impl Pool {
    pub fn outcome_mint(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.yes_mint
        } else {
            self.no_mint
        }
    }

    pub fn share_mint(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.yes_share_mint
        } else {
            self.no_share_mint
        }
    }

    pub fn pool_outcome_account(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.pool_yes
        } else {
            self.pool_no
        }
    }

    pub fn is_open(&self) -> bool {
        self.status == PoolStatus::Open
    }
}
