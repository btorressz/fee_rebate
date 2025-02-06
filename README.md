# fee_rebate


## Overview

This Solana **on-chain program** implements a **maker-taker fee rebate model** for decentralized exchanges (DEXs). The contract(program) **incentivizes liquidity providers (makers)** and **charges fees to order takers** while tracking users' trading activity and distributing liquidity rewards.
This program is currently being developed in Solana Playground IDE

devnet:(https://explorer.solana.com/address/5CvaXsLiugYKb6nPUqyshDh7vHV12zZGT9t9CC152qgF?cluster=devnet)

### Key Features
- **Maker-Taker Fee Model:** 
  - Makers receive a rebate for providing liquidity.
  - Takers pay a fee for consuming liquidity.
- **On-Chain Order Tracking:**
  - Orders are stored within user accounts.
  - Supports order placement, cancellation, and partial fills.
- **Liquidity Score & Rewards:**
  - Users accumulate liquidity scores based on time-in-market.
  - Periodic liquidity rewards can be distributed.
- **Referral Program:**
  - Users can earn referral rewards from taker fees.
- **Admin Controls:**
  - Fees and rewards can be updated by the market authority.
  - Admins can withdraw collected fees.

---

