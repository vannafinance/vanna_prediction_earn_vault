//! Token-2022 helpers. Balances are read straight from account data rather than through a deserialized snapshot, so
//! a read after a CPI always sees the post-CPI balance, and an account whose mint World has since closed still reads.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token_2022::spl_token_2022::instruction as token_ix;

const ACCOUNT_AMOUNT_OFFSET: usize = 64;
const MINT_SUPPLY_OFFSET: usize = 36;
const MINT_DECIMALS_OFFSET: usize = 44;

fn read_u64(account: &AccountInfo, offset: usize) -> Result<u64> {
    let data = account.try_borrow_data()?;
    let bytes: [u8; 8] = data
        .get(offset..offset + 8)
        .ok_or(ProgramError::InvalidAccountData)?
        .try_into()
        .unwrap();
    Ok(u64::from_le_bytes(bytes))
}

pub fn amount(token_account: &AccountInfo) -> Result<u64> {
    read_u64(token_account, ACCOUNT_AMOUNT_OFFSET)
}

pub fn supply(mint: &AccountInfo) -> Result<u64> {
    read_u64(mint, MINT_SUPPLY_OFFSET)
}

pub fn decimals(mint: &AccountInfo) -> Result<u8> {
    let data = mint.try_borrow_data()?;
    data.get(MINT_DECIMALS_OFFSET).copied().ok_or_else(|| ProgramError::InvalidAccountData.into())
}

#[allow(clippy::too_many_arguments)]
pub fn transfer_checked<'info>(
    token_program: &AccountInfo<'info>,
    from: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let ix = token_ix::transfer_checked(
        token_program.key,
        from.key,
        mint.key,
        to.key,
        authority.key,
        &[],
        amount,
        decimals(mint)?,
    )?;
    invoke_signed(
        &ix,
        &[from.clone(), mint.clone(), to.clone(), authority.clone(), token_program.clone()],
        signer_seeds,
    )
    .map_err(Into::into)
}

pub fn mint_to<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let ix = token_ix::mint_to(token_program.key, mint.key, to.key, authority.key, &[], amount)?;
    invoke_signed(&ix, &[mint.clone(), to.clone(), authority.clone(), token_program.clone()], signer_seeds)
        .map_err(Into::into)
}

/// Burns `amount` from `account`. `authority` must be the account's owner or a delegate with enough allowance;
/// Token-2022 enforces that, which is how share redemptions inherit SPL approvals.
pub fn burn<'info>(
    token_program: &AccountInfo<'info>,
    account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let ix = token_ix::burn(token_program.key, account.key, mint.key, authority.key, &[], amount)?;
    invoke_signed(&ix, &[account.clone(), mint.clone(), authority.clone(), token_program.clone()], &[])
        .map_err(Into::into)
}
