//! An ICP stake's and an ICP withdrawal's own steps: their rules, and what they change.

use super::*;

impl PoolLedger {
    /// The rules of an ICP stake's or an ICP withdrawal's own steps.
    pub(super) fn admits_icp_op(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            PoolStep::StakedIcp {
                id,
                owner,
                amount,
                move_fee,
                ..
            } => match self.pending.get(id) {
                Some(PendingOp::Stake {
                    owner: started,
                    amount: pulled,
                    move_fee: fee,
                    ..
                }) if started == owner && pulled == amount && fee == move_fee => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            PoolStep::WithdrawnIcp { id, owner, .. } => match self.pending.get(id) {
                Some(PendingOp::Withdraw { owner: started, .. }) if started == owner => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            PoolStep::StakeStarted {
                id,
                amount,
                move_fee,
                ..
            } => {
                self.settings()?;
                if self.pending.contains_key(id) {
                    return Err(PoolStepError::Pending(*id));
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                if move_fee >= amount {
                    return Err(PoolStepError::Terms(
                        "the move fee must be below the amount",
                    ));
                }
                Ok(())
            }
            PoolStep::StakeFailed { id, .. } => match self.pending.get(id) {
                Some(PendingOp::Stake { .. }) => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            // an ICP stake whose pull's answer was lost, resolved once by a controller (its
            // door holds it to the ledger's deduplication window)
            PoolStep::StakeResolved { id, resolution } => match self.pending.get(id) {
                Some(PendingOp::Stake { .. }) => match resolution {
                    StakeResolution::Pulled { shares, .. } if *shares == Shares::ZERO => {
                        Err(PoolStepError::Zero("shares"))
                    }
                    // one ledger block resolves one stake
                    StakeResolution::Pulled { block, .. }
                        if self
                            .resolved_pulls
                            .as_ref()
                            .is_some_and(|blocks| blocks.contains(block)) =>
                    {
                        Err(PoolStepError::Terms("a ledger block resolves one stake"))
                    }
                    _ => Ok(()),
                },
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc,
                move_fee,
                ..
            } => {
                self.settings()?;
                if self.pending.contains_key(id) {
                    return Err(PoolStepError::Pending(*id));
                }
                if *usdc == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("usdc"));
                }
                if move_fee >= usdc {
                    return Err(PoolStepError::Terms("the move fee must be below usdc"));
                }
                let have = lookup.shares_of(&PoolAccount::Icp(*owner).key());
                if have < *burned {
                    return Err(PoolStepError::ShortShares {
                        have,
                        need: *burned,
                    });
                }
                let least = Shares::burn_for(*usdc, self.total_assets(), self.total_shares)
                    .ok_or(PoolStepError::Terms("the pool's assets cannot pay that"))?;
                if *burned < least {
                    return Err(PoolStepError::BurnTooSmall {
                        burned: *burned,
                        least,
                        usdc: *usdc,
                    });
                }
                // a withdrawal waits behind the queued payouts
                if *usdc > self.icp_available() {
                    return Err(PoolStepError::PocketShort {
                        free: self.icp_available(),
                        need: *usdc,
                    });
                }
                Ok(())
            }
            _ => Err(unrouted(step)),
        }
    }

    /// What an ICP stake's or an ICP withdrawal's own steps change.
    pub(super) fn apply_icp_op(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        effects: &mut PoolEffects,
    ) {
        match step {
            PoolStep::StakedIcp {
                id,
                owner,
                amount,
                move_fee,
                shares,
                ..
            } => {
                self.pending.remove(id);
                self.book_stake(*amount, *move_fee, *shares);
                effects.shares = Some(credit_shares(lookup, owner, *shares));
            }
            PoolStep::StakeResolved { id, resolution } => {
                match (self.pending.remove(id), resolution) {
                    // the pull landed: booked as `StakedIcp` books it
                    (
                        Some(PendingOp::Stake {
                            owner,
                            amount,
                            move_fee,
                            ..
                        }),
                        StakeResolution::Pulled { block, shares },
                    ) => {
                        self.book_stake(amount, move_fee, *shares);
                        self.resolved_pulls
                            .get_or_insert_with(BTreeSet::new)
                            .insert(*block);
                        effects.shares = Some(credit_shares(lookup, &owner, *shares));
                    }
                    // it never landed: dropped as `StakeFailed` drops it
                    (Some(PendingOp::Stake { amount, .. }), StakeResolution::NotPulled) => {
                        self.stake_room = sat_sub(self.stake_room, amount);
                    }
                    _ => {}
                }
            }
            PoolStep::WithdrawnIcp { id, spent, .. } => {
                if let Some(PendingOp::Withdraw { usdc, .. }) = self.pending.remove(id) {
                    self.owed = sat_sub(self.owed, usdc);
                    self.spend_reserved(usdc, *spent);
                }
            }
            PoolStep::StakeStarted {
                id,
                owner,
                from,
                amount,
                created_at,
                move_fee,
            } => {
                self.stake_room = sat_add(self.stake_room, *amount);
                self.pending.insert(
                    *id,
                    PendingOp::Stake {
                        owner: *owner,
                        from: *from,
                        amount: *amount,
                        created_at: *created_at,
                        move_fee: *move_fee,
                    },
                );
            }
            PoolStep::StakeFailed { id, .. } => {
                if let Some(PendingOp::Stake { amount, .. }) = self.pending.remove(id) {
                    self.stake_room = sat_sub(self.stake_room, amount);
                }
            }
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc,
                to,
                token_out,
                min_out,
                attempt,
                move_fee,
            } => {
                let account = PoolAccount::Icp(*owner);
                let line = ShareLine {
                    account,
                    shares: lookup
                        .shares_of(&account.key())
                        .checked_sub(*burned)
                        .unwrap_or(Shares::ZERO),
                };
                effects.shares = Some((account.key(), line));
                self.total_shares = self
                    .total_shares
                    .checked_sub(*burned)
                    .unwrap_or(Shares::ZERO);
                // the stakers lose all of `usdc`: the move fee is Swapic's at once, the rest
                // reserved for the payout
                let budget = sat_sub(*usdc, *move_fee);
                let icp = &mut self.pockets.icp;
                icp.free = sat_sub(icp.free, *usdc);
                icp.reserved = sat_add(icp.reserved, budget);
                self.owed = sat_add(self.owed, budget);
                let mut lines = self.lines();
                lines.fees_icp = sat_add(lines.fees_icp, *move_fee);
                self.payouts = Some(lines);
                self.pending.insert(
                    *id,
                    PendingOp::Withdraw {
                        owner: *owner,
                        burned: *burned,
                        usdc: budget,
                        to: *to,
                        token_out: *token_out,
                        min_out: *min_out,
                        payout: PayoutProgress {
                            attempt: Some(*attempt),
                            ..PayoutProgress::default()
                        },
                    },
                );
            }
            _ => unrouted_apply(step),
        }
    }

    /// An ICP stake's pull landed: its room taken back, its ckUSDC free on ICP but the move
    /// fee, which is on the ICP fee line, its shares outstanding.
    fn book_stake(&mut self, amount: TokenAmount, move_fee: TokenAmount, shares: Shares) {
        self.stake_room = sat_sub(self.stake_room, amount);
        self.pockets.icp.free = sat_add(self.pockets.icp.free, sat_sub(amount, move_fee));
        let mut lines = self.lines();
        lines.fees_icp = sat_add(lines.fees_icp, move_fee);
        self.payouts = Some(lines);
        self.total_shares = self
            .total_shares
            .checked_add(shares)
            .unwrap_or(Shares::new(u128::MAX));
    }
}
