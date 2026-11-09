//! The Solana lines' fold (design 5.2.3). Attempts, finishes, co-signs and proposals are read by
//! the doors and written only here, from the log.

#[cfg(test)]
mod tests;

use super::{State, Store};
use types::solana::{
    SolanaEntry, SolanaKey, SolanaProposalStatus, SolanaPurpose, SolanaRef, SolanaStep,
};
use types::{EventIndex, Timestamp, TransitionError};

/// The hour a time falls in: the buckets of the co-sign limits.
pub fn hour_of(seconds: u64) -> u64 {
    seconds / 3600
}

impl<S: Store> State<S> {
    fn solana_count(&self, key: &SolanaKey) -> u64 {
        match self.store.solana_entry(key) {
            Some(SolanaEntry::Count(n)) => n,
            _ => 0,
        }
    }

    fn solana_add(&mut self, key: SolanaKey) {
        let n = self.solana_count(&key);
        self.store
            .put_solana_entry(key, SolanaEntry::Count(n.saturating_add(1)));
    }

    pub fn solana_complete_attempts(&self, arrival: &SolanaRef) -> u64 {
        self.solana_count(&SolanaKey::complete_attempts(arrival))
    }

    pub fn solana_release_attempts(&self, arrival: &SolanaRef) -> u64 {
        self.solana_count(&SolanaKey::release_attempts(arrival))
    }

    pub fn solana_forward_attempts(&self, arrival: &SolanaRef) -> u64 {
        self.solana_count(&SolanaKey::forward_attempts(arrival))
    }

    pub fn solana_arrival_done(&self, arrival: &SolanaRef) -> bool {
        self.store
            .solana_entry(&SolanaKey::arrival_done(arrival))
            .is_some()
    }

    /// The arrival a ref finished as: one finish per `(domain 5, ref)`.
    pub fn solana_finished_ref(&self, r#ref: &SolanaRef) -> Option<SolanaRef> {
        match self.store.solana_entry(&SolanaKey::finished_ref(r#ref)) {
            Some(SolanaEntry::Finished { arrival }) => Some(arrival),
            _ => None,
        }
    }

    /// `(message hash, line, when, signer)` of a ref's co-signature.
    pub fn solana_cosign_of_ref(
        &self,
        r#ref: &SolanaRef,
    ) -> Option<([u8; 32], u64, u64, [u8; 32])> {
        match self.store.solana_entry(&SolanaKey::cosign_by_ref(r#ref)) {
            Some(SolanaEntry::Cosigned {
                message_hash,
                line,
                at_s,
                signer,
            }) => Some((message_hash, line, at_s, signer)),
            _ => None,
        }
    }

    /// The signer of a transaction the canister co-signed, by its signature.
    pub fn solana_cosign_signer(&self, signature: &[u8; 64]) -> Option<[u8; 32]> {
        match self
            .store
            .solana_entry(&SolanaKey::cosign_by_signature(signature))
        {
            Some(SolanaEntry::Cosigned { signer, .. }) => Some(signer),
            _ => None,
        }
    }

    /// `(blocked, succeeded, co-signs this hour)` of a signer.
    pub fn solana_signer(&self, signer: &[u8; 32], hour: u64) -> (bool, bool, u32) {
        match self.store.solana_entry(&SolanaKey::signer(signer)) {
            Some(SolanaEntry::Signer {
                blocked,
                succeeded,
                hour: at,
                count,
            }) => (blocked, succeeded, if at == hour { count } else { 0 }),
            _ => (false, false, 0),
        }
    }

    /// `(co-signs, landed failures)` of all signers this hour.
    pub fn solana_global(&self, hour: u64) -> (u32, u32) {
        match self.store.solana_entry(&SolanaKey::global()) {
            Some(SolanaEntry::Global {
                hour: at,
                count,
                failures,
            }) if at == hour => (count, failures),
            _ => (0, 0),
        }
    }

    /// `(when, reclaims signed)` of a burn the keeper may reclaim.
    pub fn solana_burn(&self, r#ref: &SolanaRef) -> Option<(u64, u32)> {
        match self.store.solana_entry(&SolanaKey::burn(r#ref)) {
            Some(SolanaEntry::Burn {
                at_s,
                reclaim_tries,
            }) => Some((at_s, reclaim_tries)),
            _ => None,
        }
    }

    pub fn solana_proposal(&self, id: u64) -> Option<types::solana::SolanaProposal> {
        match self.store.solana_entry(&SolanaKey::proposal(id)) {
            Some(SolanaEntry::Proposal(proposal)) => Some(proposal),
            _ => None,
        }
    }

    pub fn solana_proposal_count(&self) -> u64 {
        self.solana_count(&SolanaKey::proposal_count())
    }

    /// The proposal waiting out its delay: one is live at a time.
    pub fn solana_live_proposal(&self) -> Option<types::solana::SolanaProposal> {
        let count = self.solana_proposal_count();
        let last = self.solana_proposal(count.checked_sub(1)?)?;
        (last.status == SolanaProposalStatus::Waiting).then_some(last)
    }

    pub fn solana_last_top_up(&self) -> Option<u64> {
        match self.store.solana_entry(&SolanaKey::last_top_up()) {
            Some(SolanaEntry::Time(at)) => Some(at),
            _ => None,
        }
    }

    /// What a line must satisfy before it enters the log.
    pub(crate) fn admits_solana(&self, step: &SolanaStep) -> Result<(), TransitionError> {
        let refuse = |why: &'static str| Err(TransitionError::Solana { why });
        match step {
            SolanaStep::RefFinished { r#ref, arrival, .. } => match self.solana_finished_ref(r#ref)
            {
                Some(finished) if finished != *arrival => {
                    refuse("the ref finished as another arrival")
                }
                _ => Ok(()),
            },
            SolanaStep::UpgradeProposed { proposal } => {
                if self.solana_live_proposal().is_some() {
                    return refuse("a proposal is live");
                }
                if proposal.id != self.solana_proposal_count() {
                    return refuse("the proposal's id is not the next");
                }
                Ok(())
            }
            SolanaStep::UpgradeCancelled { id } | SolanaStep::UpgradeExecuted { id } => {
                match self.solana_live_proposal() {
                    Some(live) if live.id == *id => Ok(()),
                    _ => refuse("the proposal is not the live one"),
                }
            }
            SolanaStep::Signed {
                purpose:
                    SolanaPurpose::Cosign {
                        r#ref,
                        message_hash,
                        ..
                    },
                ..
            } => match self.solana_cosign_of_ref(r#ref) {
                Some((hash, ..)) if hash == *message_hash => refuse("this message was co-signed"),
                _ => Ok(()),
            },
            SolanaStep::KeysDerived { .. }
            | SolanaStep::Signed { .. }
            | SolanaStep::CosignFailed { .. }
            | SolanaStep::ArrivalFinished { .. }
            | SolanaStep::Misrouted { .. } => Ok(()),
        }
    }

    /// Applies a line. Refuses nothing: `admits_solana` already did.
    pub(crate) fn record_solana(
        &mut self,
        step: &SolanaStep,
        index: EventIndex,
        timestamp: Timestamp,
    ) {
        let now_s = timestamp.as_secs().get();
        match step {
            SolanaStep::KeysDerived { .. } | SolanaStep::Misrouted { .. } => {}
            SolanaStep::Signed {
                purpose, signature, ..
            } => {
                self.record_signed(index);
                match purpose {
                    SolanaPurpose::Cosign {
                        r#ref,
                        signer,
                        message_hash,
                    } => {
                        let entry = SolanaEntry::Cosigned {
                            message_hash: *message_hash,
                            line: index.get(),
                            at_s: now_s,
                            signer: *signer,
                        };
                        self.store
                            .put_solana_entry(SolanaKey::cosign_by_ref(r#ref), entry.clone());
                        self.store
                            .put_solana_entry(SolanaKey::cosign_by_signature(signature), entry);
                        self.note_burn(r#ref, now_s);
                        self.note_cosign(signer, hour_of(now_s));
                    }
                    SolanaPurpose::Complete { arrival } => {
                        self.solana_add(SolanaKey::complete_attempts(arrival))
                    }
                    SolanaPurpose::Release { arrival }
                    | SolanaPurpose::MisroutedRelease { arrival } => {
                        self.solana_add(SolanaKey::release_attempts(arrival))
                    }
                    SolanaPurpose::ForwardRelay { arrival } => {
                        self.solana_add(SolanaKey::forward_attempts(arrival))
                    }
                    SolanaPurpose::Reclaim { refs } => {
                        for r#ref in refs {
                            if let Some((at_s, tries)) = self.solana_burn(r#ref) {
                                self.store.put_solana_entry(
                                    SolanaKey::burn(r#ref),
                                    SolanaEntry::Burn {
                                        at_s,
                                        reclaim_tries: tries.saturating_add(1),
                                    },
                                );
                            }
                        }
                    }
                    SolanaPurpose::TopUp => {
                        self.store
                            .put_solana_entry(SolanaKey::last_top_up(), SolanaEntry::Time(now_s));
                    }
                    SolanaPurpose::Config
                    | SolanaPurpose::Upgrade { .. }
                    | SolanaPurpose::Sweep
                    | SolanaPurpose::Proof => {}
                }
            }
            SolanaStep::CosignFailed { signer, .. } => {
                let hour = hour_of(now_s);
                let (_, succeeded, count) = self.solana_signer(signer, hour);
                self.store.put_solana_entry(
                    SolanaKey::signer(signer),
                    SolanaEntry::Signer {
                        blocked: true,
                        succeeded,
                        hour,
                        count,
                    },
                );
                let (count, failures) = self.solana_global(hour);
                self.store.put_solana_entry(
                    SolanaKey::global(),
                    SolanaEntry::Global {
                        hour,
                        count,
                        failures: failures.saturating_add(1),
                    },
                );
            }
            SolanaStep::UpgradeProposed { proposal } => {
                self.store.put_solana_entry(
                    SolanaKey::proposal(proposal.id),
                    SolanaEntry::Proposal(proposal.clone()),
                );
                self.solana_add(SolanaKey::proposal_count());
            }
            SolanaStep::UpgradeCancelled { id } => {
                self.close_proposal(*id, SolanaProposalStatus::Cancelled)
            }
            SolanaStep::UpgradeExecuted { id } => {
                self.close_proposal(*id, SolanaProposalStatus::Executed)
            }
            SolanaStep::RefFinished {
                r#ref,
                arrival,
                burned_at_s,
            } => {
                self.store.put_solana_entry(
                    SolanaKey::finished_ref(r#ref),
                    SolanaEntry::Finished { arrival: *arrival },
                );
                self.note_burn(r#ref, *burned_at_s);
                // a co-signed burn that reached its finish landed: its signer has an earlier success
                if let Some((.., signer)) = self.solana_cosign_of_ref(r#ref) {
                    let hour = hour_of(now_s);
                    let (blocked, _, count) = self.solana_signer(&signer, hour);
                    self.store.put_solana_entry(
                        SolanaKey::signer(&signer),
                        SolanaEntry::Signer {
                            blocked,
                            succeeded: true,
                            hour,
                            count,
                        },
                    );
                }
            }
            SolanaStep::ArrivalFinished { arrival } => {
                self.store
                    .put_solana_entry(SolanaKey::arrival_done(arrival), SolanaEntry::Done);
            }
        }
    }

    fn note_burn(&mut self, r#ref: &SolanaRef, at_s: u64) {
        if self.solana_burn(r#ref).is_none() {
            self.store.put_solana_entry(
                SolanaKey::burn(r#ref),
                SolanaEntry::Burn {
                    at_s,
                    reclaim_tries: 0,
                },
            );
        }
    }

    fn note_cosign(&mut self, signer: &[u8; 32], hour: u64) {
        let (blocked, succeeded, count) = self.solana_signer(signer, hour);
        self.store.put_solana_entry(
            SolanaKey::signer(signer),
            SolanaEntry::Signer {
                blocked,
                succeeded,
                hour,
                count: count.saturating_add(1),
            },
        );
        let (count, failures) = self.solana_global(hour);
        self.store.put_solana_entry(
            SolanaKey::global(),
            SolanaEntry::Global {
                hour,
                count: count.saturating_add(1),
                failures,
            },
        );
    }

    fn close_proposal(&mut self, id: u64, status: SolanaProposalStatus) {
        if let Some(mut proposal) = self.solana_proposal(id) {
            proposal.status = status;
            self.store
                .put_solana_entry(SolanaKey::proposal(id), SolanaEntry::Proposal(proposal));
        }
    }
}
