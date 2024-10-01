mod cctp;
pub use cctp::*;

mod local;
pub use local::*;

use crate::{
    composite::*,
    events::AuctionSettled,
    state::{Auction, AuctionStatus, PreparedOrderResponse},
};
use anchor_lang::prelude::*;
use anchor_spl::token;
use common::messages::Fill;

struct SettleNoneAndPrepareFill<'ctx, 'info> {
    prepared_order_response: &'ctx mut Account<'info, PreparedOrderResponse>,
    prepared_custody_token: &'ctx Account<'info, token::TokenAccount>,
    auction: &'ctx mut Account<'info, Auction>,
    fee_recipient_token: &'ctx Account<'info, token::TokenAccount>,
    custodian: &'ctx CheckedCustodian<'info>,
    token_program: &'ctx Program<'info, token::Token>,
}

struct SettledNone {
    user_amount: u64,
    fill: Fill,
    auction_settled_event: AuctionSettled,
}

fn settle_none_and_prepare_fill(accounts: SettleNoneAndPrepareFill<'_, '_>) -> Result<SettledNone> {
    let SettleNoneAndPrepareFill {
        prepared_order_response,
        prepared_custody_token,
        auction,
        fee_recipient_token,
        custodian,
        token_program,
    } = accounts;

    let prepared_order_response_signer_seeds = &[
        PreparedOrderResponse::SEED_PREFIX,
        prepared_order_response.seeds.fast_vaa_hash.as_ref(),
        &[prepared_order_response.seeds.bump],
    ];

    // Pay the `fee_recipient` the base fee and init auction fee. This ensures that the protocol
    // relayer is paid for relaying slow VAAs (which requires posting the fast order VAA) that do
    // not have an associated auction.
    let fee = prepared_order_response
        .base_fee
        .saturating_add(prepared_order_response.init_auction_fee);
    token::transfer(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            token::Transfer {
                from: prepared_custody_token.to_account_info(),
                to: fee_recipient_token.to_account_info(),
                authority: prepared_order_response.to_account_info(),
            },
            &[prepared_order_response_signer_seeds],
        ),
        fee,
    )?;

    // Set the authority of the custody token account to the custodian. He will take over from here.
    token::set_authority(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            token::SetAuthority {
                current_authority: prepared_order_response.to_account_info(),
                account_or_mint: prepared_custody_token.to_account_info(),
            },
            &[prepared_order_response_signer_seeds],
        ),
        token::spl_token::instruction::AuthorityType::AccountOwner,
        custodian.key().into(),
    )?;

    // Indicate that the auction has been settled.
    auction.status = AuctionStatus::Settled {
        fee,
        total_penalty: None,
    };

    let auction_settled_event = AuctionSettled {
        fast_vaa_hash: auction.vaa_hash,
        best_offer_token: Default::default(),
        base_fee_token: crate::events::SettledTokenAccountInfo {
            key: fee_recipient_token.key(),
            balance_after: fee_recipient_token.amount.saturating_add(fee),
        }
        .into(),
        with_execute: auction.target_protocol.into(),
    };

    // TryInto is safe to unwrap here because the redeemer message had to have been able to fit in
    // the prepared order response account (so it would not have exceed u32::MAX).
    let redeemer_message = std::mem::take(&mut prepared_order_response.redeemer_message)
        .try_into()
        .unwrap();
    Ok(SettledNone {
        user_amount: prepared_custody_token.amount.saturating_sub(fee),
        fill: Fill {
            source_chain: prepared_order_response.source_chain,
            order_sender: prepared_order_response.sender,
            redeemer: prepared_order_response.redeemer,
            redeemer_message,
        },
        auction_settled_event,
    })
}
