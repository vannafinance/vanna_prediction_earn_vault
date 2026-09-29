//! Share/asset conversions for one side of a market.
//!
//! Both conversions round down, so rounding always favours the pool over the caller. Intermediates are u128 and every
//! step is checked, so an out-of-range result is an error rather than a wrap.

use crate::errors::PoolError;
use anchor_lang::prelude::*;

/// Virtual shares and assets added to the totals in every conversion, so an empty side has a defined and
/// un-manipulable share price. Supplies are u64, and `1e3` keeps a side's share supply within u64 up to ~1.8e16 raw
/// CASH units (~18 billion CASH at 6 decimals).
pub const VIRTUAL_SHARES: u128 = 1_000;
pub const VIRTUAL_ASSETS: u128 = 1;

fn mul_div(a: u64, b: u128, c: u128) -> Result<u64> {
    let product = (a as u128).checked_mul(b).ok_or_else(|| error!(PoolError::MathOverflow))?;
    u64::try_from(product / c).map_err(|_| error!(PoolError::MathOverflow))
}

/// Shares minted for `assets`, priced against the side's totals before the deposit.
pub fn to_shares_down(assets: u64, total_assets: u64, total_shares: u64) -> Result<u64> {
    mul_div(assets, total_shares as u128 + VIRTUAL_SHARES, total_assets as u128 + VIRTUAL_ASSETS)
}

/// Assets owed for `shares`, priced against the side's current totals.
pub fn to_assets_down(shares: u64, total_assets: u64, total_shares: u64) -> Result<u64> {
    mul_div(shares, total_assets as u128 + VIRTUAL_ASSETS, total_shares as u128 + VIRTUAL_SHARES)
}

pub fn checked_add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| error!(PoolError::MathOverflow))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_mints_virtual_shares_per_asset() {
        assert_eq!(to_shares_down(1_000_000, 0, 0).unwrap(), 1_000_000_000);
    }

    #[test]
    fn round_trip_never_pays_out_more_than_deposited() {
        for (assets, total_assets, total_shares) in
            [(1u64, 0u64, 0u64), (7, 13, 11_000), (1_000_000, 5_000_000, 4_000_000_000), (u32::MAX as u64, 3, 3)]
        {
            let shares = to_shares_down(assets, total_assets, total_shares).unwrap();
            let back = to_assets_down(shares, total_assets + assets, total_shares + shares).unwrap();
            assert!(back <= assets, "round trip paid {back} for {assets}");
        }
    }

    #[test]
    fn yield_raises_the_asset_value_of_a_share() {
        let shares = to_shares_down(100, 0, 0).unwrap();
        assert!(to_assets_down(shares, 105, shares).unwrap() >= 104);
    }

    #[test]
    fn overflowing_result_is_an_error_not_a_wrap() {
        assert!(to_shares_down(u64::MAX, 0, u64::MAX).is_err());
    }
}
