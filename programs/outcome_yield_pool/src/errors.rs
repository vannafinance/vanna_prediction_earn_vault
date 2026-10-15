use anchor_lang::prelude::*;

#[error_code]
pub enum PoolError {
    #[msg("Signer is not the config admin")]
    Unauthorized,
    #[msg("Signer is not the config resolver")]
    NotResolver,
    #[msg("Account is not a prediCt Market")]
    NotPredictMarket,
    #[msg("Market collateral is not CASH")]
    InvalidCollateral,
    #[msg("Mint does not match the market's outcome mint")]
    OutcomeMintMismatch,
    #[msg("Account is not the pool's Vanna reserve or one of its accounts")]
    VannaAccountMismatch,
    #[msg("Account is not Vanna's USDC reserve")]
    InvalidVannaReserve,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Deposit is too small to mint a share")]
    ZeroShares,
    #[msg("Pool has already been settled")]
    PoolSettled,
    #[msg("Pool has not been settled yet")]
    PoolNotSettled,
    #[msg("Market has not been resolved yet")]
    MarketNotResolved,
    #[msg("Market resolved to a non-binary payout, which the pool does not support")]
    UnsupportedOutcome,
    #[msg("Market account still exists; settle from it instead")]
    MarketStillOpen,
    #[msg("prediCt merge returned a different amount of CASH than it burned")]
    MergeAmountMismatch,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Market has been resolved; settle the pool instead")]
    MarketResolved,
    #[msg("Swap is not an allowed Jupiter route")]
    SwapNotAllowed,
    #[msg("Swap accounts reach pool funds outside the swap")]
    InvalidSwapAccounts,
    #[msg("Swap moved funds other than it declared")]
    InvalidSwapResult,
    #[msg("Swap input does not match what the pool needs to convert")]
    SwapAmountMismatch,
    #[msg("Swap returned less than the pool accepts")]
    SwapBelowMinimum,
    #[msg("Swap loss tolerance exceeds the cap")]
    InvalidSwapPolicy,
    #[msg("Market's CASH vault belongs to a World Group; use merge_and_deposit_in_group")]
    MarketInGroup,
    #[msg("Group does not own the market's CASH vault")]
    GroupMismatch,
}
