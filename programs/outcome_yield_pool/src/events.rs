use anchor_lang::prelude::*;

#[event]
pub struct MarketAllowed {
    pub market: Pubkey,
}

#[event]
pub struct MarketDisallowed {
    pub market: Pubkey,
}

#[event]
pub struct SwapPolicySet {
    pub keeper: Pubkey,
    pub max_swap_loss_bps: u16,
}

#[event]
pub struct PoolCreated {
    pub pool: Pubkey,
    pub market: Pubkey,
    pub vanna_reserve: Pubkey,
    pub yes_share_mint: Pubkey,
    pub no_share_mint: Pubkey,
}

#[event]
pub struct Deposit {
    pub pool: Pubkey,
    pub is_yes: bool,
    pub caller: Pubkey,
    /// Owner of the share account the shares were minted into.
    pub to: Pubkey,
    pub assets: u64,
    pub shares: u64,
}

#[event]
pub struct MergeAndDeposit {
    pub pool: Pubkey,
    pub complete_sets: u64,
    /// USDC supplied to Vanna: the swap's output plus any USDC that was idle in the pool.
    pub usdc_supplied: u64,
}

/// One CASH <-> USDC swap through Jupiter. `amount_out - amount_in` is what the pool gained (or lost) against par.
#[event]
pub struct Swapped {
    pub pool: Pubkey,
    pub cash_to_usdc: bool,
    pub caller: Pubkey,
    pub amount_in: u64,
    pub amount_out: u64,
}

#[event]
pub struct Redeem {
    pub pool: Pubkey,
    pub is_yes: bool,
    pub caller: Pubkey,
    /// Owner of the share account the shares were burned from.
    pub on_behalf: Pubkey,
    /// Owner of the token account the outcome tokens were paid into.
    pub to: Pubkey,
    pub shares: u64,
    pub assets: u64,
}

#[event]
pub struct PoolSettled {
    pub pool: Pubkey,
    pub yes_won: bool,
    /// True when the resolver settled a pool whose market account was already closed.
    pub by_resolver: bool,
}

#[event]
pub struct RedeemSettled {
    pub pool: Pubkey,
    pub is_yes: bool,
    pub caller: Pubkey,
    pub on_behalf: Pubkey,
    pub shares: u64,
    /// Outcome tokens paid out.
    pub tokens_out: u64,
    /// CASH paid out.
    pub cash_out: u64,
    /// USDC paid out.
    pub usdc_out: u64,
}
