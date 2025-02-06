use anchor_lang::prelude::*;
use anchor_lang::solana_program::{system_program, sysvar};

declare_id!("5CvaXsLiugYKb6nPUqyshDh7vHV12zZGT9t9CC152qgF"); 
// ----------------------------------
// PROGRAM
// ----------------------------------

#[program]
pub mod fee_rebate {
    use super::*;

    /// Initialize the market with default fee parameters and referral incentives.
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        maker_rebate_bps: u16,
        taker_fee_bps: u16,
        referral_bps: u16,
    ) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        
        // Validate fee config
        require!(
            maker_rebate_bps <= taker_fee_bps,
            FeeError::InvalidFeeConfiguration
        );
        require!(
            referral_bps <= taker_fee_bps,
            FeeError::InvalidFeeConfiguration
        );

        market_state.authority = *ctx.accounts.authority.key;
        market_state.maker_rebate_bps = maker_rebate_bps;
        market_state.taker_fee_bps = taker_fee_bps;
        market_state.referral_bps = referral_bps;
        market_state.total_fees_collected = 0;
        market_state.total_liquidity_rewards_distributed = 0;

        Ok(())
    }

    /// Allows the market authority to update fee parameters at any time.
    pub fn update_fee_parameters(
        ctx: Context<UpdateFeeParameters>,
        new_maker_rebate_bps: u16,
        new_taker_fee_bps: u16,
        new_referral_bps: u16,
    ) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        require!(
            market_state.authority == *ctx.accounts.authority.key,
            FeeError::Unauthorized
        );

        require!(
            new_maker_rebate_bps <= new_taker_fee_bps,
            FeeError::InvalidFeeConfiguration
        );
        require!(
            new_referral_bps <= new_taker_fee_bps,
            FeeError::InvalidFeeConfiguration
        );

        market_state.maker_rebate_bps = new_maker_rebate_bps;
        market_state.taker_fee_bps = new_taker_fee_bps;
        market_state.referral_bps = new_referral_bps;

        emit!(FeeParametersUpdated {
            maker_rebate_bps: new_maker_rebate_bps,
            taker_fee_bps: new_taker_fee_bps,
            referral_bps: new_referral_bps,
        });

        Ok(())
    }

    /// Register a user in this market, creating a PDA that tracks:
    ///   - Orders
    ///   - Maker/taker stats
    ///   - Referral relationship
    ///   - Liquidity score
    pub fn register_user(
        ctx: Context<RegisterUser>,
        referrer: Option<Pubkey>,
    ) -> Result<()> {
        let user_state = &mut ctx.accounts.user_state;

        user_state.authority = *ctx.accounts.user_authority.key;
        user_state.maker_volume = 0;
        user_state.taker_volume = 0;
        user_state.maker_rebates_earned = 0;
        user_state.taker_fees_paid = 0;
        user_state.liquidity_score = 0;
        user_state.referrer = referrer;
        user_state.orders = [Order::default(); MAX_ORDERS];

        Ok(())
    }

    /// Place an order with details. For simplicity, store a maximum of `MAX_ORDERS` per user.
    /// This demonstrates partial fills, time-in-force, etc.
    pub fn place_order(
        ctx: Context<PlaceOrder>,
        side: OrderSide,
        price: u64,
        size: u64,
        expiry_timestamp: i64, // if 0, treat as no expiry
    ) -> Result<()> {
        let user_state = &mut ctx.accounts.user_state;
        require!(
            user_state.authority == *ctx.accounts.user_authority.key,
            FeeError::Unauthorized
        );

        let now = Clock::get()?.unix_timestamp;

        //  Find an empty slot index
        let mut free_slot_index = None;
        for (i, order_slot) in user_state.orders.iter().enumerate() {
            if order_slot.size_remaining == 0 {
                free_slot_index = Some(i);
                break;
            }
        }

        //  If none found, error out
        require!(free_slot_index.is_some(), FeeError::NoFreeOrderSlot);

        //  Write the new order data at that slot
        let idx = free_slot_index.unwrap();
        user_state.orders[idx] = Order {
            side,
            price,
            size_remaining: size,
            creation_timestamp: now,
            expiry_timestamp,
        };

        //  Emit an event (no longer holding a mutable reference to the array slot)
        emit!(OrderPlaced {
            user: user_state.authority,
            side,
            price,
            size,
            expiry_timestamp,
        });

        Ok(())
    }

    /// Cancel a specific order by index. This frees up the slot.
    ///   also "reward" the user’s liquidity_score based on how long the order was live.
    pub fn cancel_order(
        ctx: Context<CancelOrder>,
        order_index: u8,
    ) -> Result<()> {
        let user_state = &mut ctx.accounts.user_state;
        require!(
            user_state.authority == *ctx.accounts.user_authority.key,
            FeeError::Unauthorized
        );

        let now = Clock::get()?.unix_timestamp;
        require!(
            (order_index as usize) < user_state.orders.len(),
            FeeError::InvalidOrderIndex
        );

        // Copy out relevant order data from the slot (and reset it) in a smaller scope
        let (canceled_size, added_liq) = {
            let order = &mut user_state.orders[order_index as usize];
            require!(order.size_remaining > 0, FeeError::NoOpenOrders);

            // how long it was active
            let active_time = now.checked_sub(order.creation_timestamp).unwrap_or(0);
            let added_liq = active_time
                .saturating_mul(order.size_remaining as i64)
                .max(0) as u64;

            let canceled_size = order.size_remaining;

            // Mark slot as free
            *order = Order::default();

            (canceled_size, added_liq)
        };

        //  Now that it no longer holds a mutable reference to orders[...], 
        //     can safely mutate other fields or emit events.
        user_state.liquidity_score = user_state
            .liquidity_score
            .saturating_add(added_liq);

        emit!(OrderCanceled {
            user: user_state.authority,
            order_index,
            canceled_size,
        });

        Ok(())
    }

    /// Fill a maker’s order partially or fully. Taker pays fees, maker gets rebates,
    /// referrer gets a small cut, and update liquidity scores.
    pub fn fill_order(
        ctx: Context<FillOrder>,
        maker_order_index: u8,
        fill_size: u64,
    ) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let maker_user = &mut ctx.accounts.maker_user;
        let taker_user = &mut ctx.accounts.taker_user;

        // Check signers
        require!(
            taker_user.authority == *ctx.accounts.taker_authority.key,
            FeeError::Unauthorized
        );
        require!(
            (maker_order_index as usize) < maker_user.orders.len(),
            FeeError::InvalidOrderIndex
        );

        let now = Clock::get()?.unix_timestamp;

        //  Access the maker's order in a smaller scope
        let (trade_size, maker_rebate, taker_fee, referral_reward, net_fee, fully_filled) = {
            let maker_order = &mut maker_user.orders[maker_order_index as usize];
            require!(maker_order.size_remaining > 0, FeeError::NoOpenOrders);

            // Check if order expired
            if maker_order.expiry_timestamp > 0 && now > maker_order.expiry_timestamp {
                return err!(FeeError::OrderExpired);
            }

            let actual_fill = fill_size.min(maker_order.size_remaining);

            // Fee/Rebate Calculation
            let taker_fee = (actual_fill as u128)
                .checked_mul(market_state.taker_fee_bps as u128)
                .ok_or(FeeError::Overflow)? / 10_000;

            let maker_rebate = (actual_fill as u128)
                .checked_mul(market_state.maker_rebate_bps as u128)
                .ok_or(FeeError::Overflow)? / 10_000;

            let net_fee = taker_fee
                .checked_sub(maker_rebate)
                .ok_or(FeeError::NegativeFee)?;

            // Referral
            let mut referral_reward = 0_u128;
            if let Some(_referrer_pubkey) = taker_user.referrer {
                if market_state.referral_bps > 0 {
                    referral_reward = (actual_fill as u128)
                        .checked_mul(market_state.referral_bps as u128)
                        .ok_or(FeeError::Overflow)? / 10_000;
                }
                // TODO: place credit the referrer account here.
            }

            // Reduce maker’s size_remaining
            maker_order.size_remaining = maker_order
                .size_remaining
                .checked_sub(actual_fill)
                .ok_or(FeeError::Overflow)?;

            // Check if fully filled
            let fully_filled = maker_order.size_remaining == 0;

            (
                actual_fill,        // trade_size
                maker_rebate,       // maker_rebate
                taker_fee,          // taker_fee
                referral_reward,    // referral_reward
                net_fee,            // net_fee
                fully_filled,       // fully_filled
            )
        };

        //   Now that it no longer has a reference to maker_order,  can safely
        //    update the user accounts & global market state:
        //    - maker/taker volumes, 
        //    - total_fees_collected, 
        //    - liquidity_score if fully filled, etc.
        
        // Update maker stats
        maker_user.maker_volume = maker_user
            .maker_volume
            .checked_add(trade_size)
            .ok_or(FeeError::Overflow)?;
        maker_user.maker_rebates_earned = maker_user
            .maker_rebates_earned
            .checked_add(maker_rebate as u64)
            .ok_or(FeeError::Overflow)?;

        // Update taker stats
        taker_user.taker_volume = taker_user
            .taker_volume
            .checked_add(trade_size)
            .ok_or(FeeError::Overflow)?;
        taker_user.taker_fees_paid = taker_user
            .taker_fees_paid
            .checked_add(taker_fee as u64)
            .ok_or(FeeError::Overflow)?;

        // Collect net fees
        market_state.total_fees_collected = market_state
            .total_fees_collected
            .checked_add(net_fee as u64)
            .ok_or(FeeError::Overflow)?;

        // If the maker's order was fully filled, increment their liquidity_score
        // based on how long the order was active.  need the original creation time:
        if fully_filled {
            // Re-borrow maker_order in a read-only fashion to get creation_timestamp
            // but do it carefully. it can do the same "copy out" trick or find it previously.
            let creation_timestamp = maker_user.orders[maker_order_index as usize].creation_timestamp;
            
            // (replaced the order's size_remaining with zero, but left creation_timestamp.)
            // Alternatively, it could store it in a local variable earlier, then zero out the slot.
            maker_user.orders[maker_order_index as usize] = Order::default();

            let active_time = now.saturating_sub(creation_timestamp);
            let added_liq = active_time.saturating_mul(trade_size as i64).max(0) as u64;
            maker_user.liquidity_score = maker_user.liquidity_score.saturating_add(added_liq);
        }

        //  Emit the fill event now that it's done with all references
        emit!(OrderFilled {
            maker: maker_user.authority,
            taker: taker_user.authority,
            trade_size,
            maker_rebate: maker_rebate as u64,
            taker_fee: taker_fee as u64,
            referral_reward: referral_reward as u64,
        });

        Ok(())
    }

    /// Distribute liquidity rewards to a specific user, proportional to their share
    /// of the global liquidity score.
    pub fn distribute_liquidity_rewards(
        ctx: Context<DistributeLiquidityRewards>,
        global_liquidity_score: u64,
        reward_pool: u64, // how many tokens  want to distribute in total
    ) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;
        let user_state = &mut ctx.accounts.user_state;

        // If user’s liquidity_score is 0 or global_score is 0, nothing happens.
        if user_state.liquidity_score == 0 || global_liquidity_score == 0 {
            return Ok(());
        }

        let user_share = (user_state.liquidity_score as u128)
            .checked_mul(reward_pool as u128)
            .ok_or(FeeError::Overflow)?
            / (global_liquidity_score as u128);

        //  just "emit" an event for demonstration. In real code, do an SPL token transfer.
        emit!(LiquidityRewardsDistributed {
            user: user_state.authority,
            distributed_amount: user_share as u64,
        });

        // Optionally track how many total tokens  distributed:
        market_state.total_liquidity_rewards_distributed = market_state
            .total_liquidity_rewards_distributed
            .saturating_add(user_share as u64);

        // Reset user’s liquidity score if desired
        user_state.liquidity_score = 0;

        Ok(())
    }

    /// Allows the market authority to withdraw accumulated fees from the program’s treasury.
    /// In real usage, you'd do an SPL token transfer here.
    pub fn withdraw_fees(ctx: Context<WithdrawFees>, amount: u64) -> Result<()> {
        let market_state = &mut ctx.accounts.market_state;

        // Check authority
        require!(
            market_state.authority == *ctx.accounts.authority.key,
            FeeError::Unauthorized
        );

        // Basic check if there are enough fees
        require!(
            market_state.total_fees_collected >= amount,
            FeeError::InsufficientFunds
        );

        market_state.total_fees_collected = market_state
            .total_fees_collected
            .checked_sub(amount)
            .ok_or(FeeError::Overflow)?;

        emit!(FeesWithdrawn {
            authority: market_state.authority,
            amount,
        });

        Ok(())
    }
}

// ----------------------------------
// ACCOUNTS
// ----------------------------------

#[derive(Accounts)]
#[instruction(maker_rebate_bps: u16, taker_fee_bps: u16, referral_bps: u16)]
pub struct InitializeMarket<'info> {
    #[account(init, payer = authority, space = 8 + MarketState::SIZE)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub authority: Signer<'info>,

    /// System Program required for account creation
    #[account(address = system_program::ID)]
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFeeParameters<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,
    #[account(signer)]
    pub authority: AccountInfo<'info>,
}

#[derive(Accounts)]
#[instruction(referrer: Option<Pubkey>)]
pub struct RegisterUser<'info> {
    #[account(
        init,
        payer = user_authority,
        space = 8 + UserState::SIZE,
        seeds = [b"user_state", user_authority.key().as_ref()],
        bump
    )]
    pub user_state: Account<'info, UserState>,

    #[account(mut)]
    pub user_authority: Signer<'info>,

    #[account(address = system_program::ID)]
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceOrder<'info> {
    #[account(mut)]
    pub user_state: Account<'info, UserState>,
    #[account(signer)]
    pub user_authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    #[account(mut)]
    pub user_state: Account<'info, UserState>,
    #[account(signer)]
    pub user_authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct FillOrder<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub maker_user: Account<'info, UserState>,

    #[account(mut)]
    pub taker_user: Account<'info, UserState>,

    #[account(signer)]
    pub taker_authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct DistributeLiquidityRewards<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(mut)]
    pub user_state: Account<'info, UserState>,
    // Possibly your authority or a governance key that decides on distribution intervals
    #[account(signer)]
    pub authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct WithdrawFees<'info> {
    #[account(mut)]
    pub market_state: Account<'info, MarketState>,

    #[account(signer)]
    pub authority: AccountInfo<'info>,
}

// ----------------------------------
// ACCOUNT DATA STRUCTS
// ----------------------------------

/// MarketState holds global info like fee rates, fee collection, etc.
#[account]
pub struct MarketState {
    pub authority: Pubkey,
    pub maker_rebate_bps: u16,       // e.g., 2 bps
    pub taker_fee_bps: u16,         // e.g., 5 bps
    pub referral_bps: u16,          // e.g., 1 bps
    pub total_fees_collected: u64,
    pub total_liquidity_rewards_distributed: u64,
}

impl MarketState {
    pub const SIZE: usize = 
          32 // authority
        + 2  // maker_rebate_bps
        + 2  // taker_fee_bps
        + 2  // referral_bps
        + 8  // total_fees_collected
        + 8; // total_liquidity_rewards_distributed
}

/// Each user’s state includes:
///   - maker/taker stats
///   - referral info
///   - liquidity score
///   - active orders array (for demonstration)
#[account]
pub struct UserState {
    pub authority: Pubkey,
    pub maker_volume: u64,
    pub taker_volume: u64,
    pub maker_rebates_earned: u64,
    pub taker_fees_paid: u64,
    pub liquidity_score: u64,
    pub referrer: Option<Pubkey>,
    pub orders: [Order; MAX_ORDERS],
}

// The array of orders must be carefully sized for the account.
// Each `Order` occupies 1 + 8 + 8 + 8 + 8 = 33 bytes. The sizing should be re-checked to ensure accuracy:
impl UserState {
    pub const SIZE: usize = 
          32  // authority
        + 8   // maker_volume
        + 8   // taker_volume
        + 8   // maker_rebates_earned
        + 8   // taker_fees_paid
        + 8   // liquidity_score
        + 1 + 32  // referrer: Option<Pubkey> => 1 + 32 bytes
        + (Order::SIZE * MAX_ORDERS);
}

// ----------------------------------
// ORDER STRUCT
// ----------------------------------

pub const MAX_ORDERS: usize = 5;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Order {
    pub side: OrderSide,
    pub price: u64,
    pub size_remaining: u64,
    pub creation_timestamp: i64,
    pub expiry_timestamp: i64,
}

impl Order {
    // side (enum) as 1 byte
    pub const SIZE: usize =
          1  // side
        + 8  // price
        + 8  // size_remaining
        + 8  // creation_timestamp
        + 8; // expiry_timestamp
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderSide {
    Bid,
    Ask,
}

impl Default for OrderSide {
    fn default() -> Self {
        OrderSide::Bid
    }
}

// ----------------------------------
// ERRORS
// ----------------------------------

#[error_code]
pub enum FeeError {
    #[msg("Overflow or underflow detected.")]
    Overflow,
    #[msg("Configuration leads to negative net fee.")]
    NegativeFee,
    #[msg("Unauthorized operation.")]
    Unauthorized,
    #[msg("No open orders found.")]
    NoOpenOrders,
    #[msg("Invalid fee configuration.")]
    InvalidFeeConfiguration,
    #[msg("Insufficient funds.")]
    InsufficientFunds,
    #[msg("No free slot to place a new order.")]
    NoFreeOrderSlot,
    #[msg("Invalid order index.")]
    InvalidOrderIndex,
    #[msg("Order is expired.")]
    OrderExpired,
}

// ----------------------------------
// EVENTS
// ----------------------------------

#[event]
pub struct FeeParametersUpdated {
    pub maker_rebate_bps: u16,
    pub taker_fee_bps: u16,
    pub referral_bps: u16,
}

#[event]
pub struct OrderPlaced {
    pub user: Pubkey,
    pub side: OrderSide,
    pub price: u64,
    pub size: u64,
    pub expiry_timestamp: i64,
}

#[event]
pub struct OrderCanceled {
    pub user: Pubkey,
    pub order_index: u8,
    pub canceled_size: u64,
}

#[event]
pub struct OrderFilled {
    pub maker: Pubkey,
    pub taker: Pubkey,
    pub trade_size: u64,
    pub maker_rebate: u64,
    pub taker_fee: u64,
    pub referral_reward: u64,
}

#[event]
pub struct FeesWithdrawn {
    pub authority: Pubkey,
    pub amount: u64,
}

#[event]
pub struct LiquidityRewardsDistributed {
    pub user: Pubkey,
    pub distributed_amount: u64,
}
