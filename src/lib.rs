//! Collator Selection pallet.
//!
//! A pallet to manage collators in a parachain.
//!
//! ## Overview
//!
//! The Collator Selection pallet manages the collators of a parachain. **Collation is _not_ a
//! secure activity** and this pallet does not implement any game-theoretic mechanisms to meet BFT
//! safety assumptions of the chosen set.
//!
//! ## Terminology
//!
//! - Collator: A parachain block producer.
//! - Bond: An amount of `Balance` _reserved_ for candidate registration.
//! - Invulnerable: An account guaranteed to be in the collator set.
//!
//! ## Implementation
//!
//! The final `Collators` are aggregated from two individual lists:
//!
//! 1. [`Invulnerables`]: a set of collators appointed by governance. These accounts will always be
//!    collators.
//! 2. [`CandidateList`]: these are *candidates to the collation task* and may or may not be elected
//!    as a final collator.
//!
//! The current implementation resolves congestion of [`CandidateList`] through a simple auction
//! mechanism. Candidates bid for the collator slots and at the end of the session, the auction ends
//! and the top candidates are selected to become collators. The number of selected candidates is
//! determined by the value of `DesiredCandidates`.
//!
//! Before the list reaches full capacity, candidates can register by placing the minimum bond
//! through `register_as_candidate`. Then, if an account wants to participate in the collator slot
//! auction, they have to replace an existing candidate by placing a greater deposit through
//! `take_candidate_slot`. Existing candidates can increase their bids through `stake`.
//!
//! At any point, an account can take the place of another account in the candidate list if they put
//! up a greater deposit than the target. While new joiners would like to deposit as little as
//! possible to participate in the auction, the replacement threat incentivizes candidates to bid as
//! close to their budget as possible in order to avoid being replaced.
//!
//! Candidates which are not on "winning" slots in the list can also decrease their deposits through
//! `stake`, but candidates who are on top slots and try to decrease their deposits will fail
//! in order to enforce auction mechanics and have meaningful bids.
//!
//! Candidates will not be allowed to get kicked or `leave_intent` if the total number of collators
//! would fall below `MinEligibleCollators`. This is to ensure that some collators will always
//! exist, i.e. someone is eligible to produce a block.
//!
//! When a new session starts, candidates with the highest deposits will be selected in order until
//! the desired number of collators is reached. Candidates can increase or decrease their deposits
//! between sessions in order to ensure they receive a slot in the collator list.
//!
//! ### Rewards
//!
//! The Collator Selection pallet maintains an on-chain account (the "Pot"). In each block, the
//! collator who authored it receives:
//!
//! - Half the value of the Pot.
//! - Half the value of the transaction fees within the block. The other half of the transaction
//!   fees are deposited into the Pot.
//!
//! To initiate rewards, an ED needs to be transferred to the pot address.
//!
//! Note: Eventually the Pot distribution may be modified as discussed in [this
//! issue](https://github.com/paritytech/statemint/issues/21#issuecomment-810481073).

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;
pub mod weights;

const LOG_TARGET: &str = "runtime::collator-staking";

#[frame_support::pallet]
pub mod pallet {
    pub use crate::weights::WeightInfo;
    use frame_support::{
        dispatch::{DispatchClass, DispatchResultWithPostInfo},
        pallet_prelude::*,
        traits::{
            Currency, EnsureOrigin, ExistenceRequirement::KeepAlive, ReservableCurrency,
            ValidatorRegistration,
        },
        BoundedVec, DefaultNoBound, PalletId,
    };
    use frame_system::{pallet_prelude::*, Config as SystemConfig};
    use pallet_session::SessionManager;
    use sp_runtime::Percent;
    use sp_runtime::{
        traits::{AccountIdConversion, Convert, Saturating, Zero},
        RuntimeDebug,
    };
    use sp_staking::SessionIndex;
    use sp_std::vec::Vec;
    use std::collections::BTreeMap;

    /// The in-code storage version.
    const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as SystemConfig>::AccountId>>::Balance;

    /// A convertor from collators id. Since this pallet does not have stash/controller, this is
    /// just identity.
    pub struct IdentityCollator;

    impl<T> sp_runtime::traits::Convert<T, Option<T>> for IdentityCollator {
        fn convert(t: T) -> Option<T> {
            Some(t)
        }
    }

    pub struct MaxDesiredCandidates<T>(PhantomData<T>);
    impl<T: Config> Get<u32> for MaxDesiredCandidates<T> {
        fn get() -> u32 {
            T::MaxCandidates::get().saturating_add(T::MaxInvulnerables::get())
        }
    }

    /// Configure the pallet by specifying the parameters and types on which it depends.
    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// Overarching event type.
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        /// The currency mechanism.
        type Currency: ReservableCurrency<Self::AccountId>;

        /// Origin that can dictate updating parameters of this pallet.
        type UpdateOrigin: EnsureOrigin<Self::RuntimeOrigin>;

        /// Account Identifier from which the internal Pot is generated.
        type PotId: Get<PalletId>;

        /// Maximum number of candidates that we should have.
        ///
        /// This does not take into account the invulnerables.
        type MaxCandidates: Get<u32>;

        /// Minimum number eligible collators. Should always be greater than zero. This includes
        /// Invulnerable collators. This ensures that there will always be one collator who can
        /// produce a block.
        type MinEligibleCollators: Get<u32>;

        /// Maximum number of invulnerables.
        type MaxInvulnerables: Get<u32>;

        // Will be kicked if block is not produced in threshold.
        type KickThreshold: Get<BlockNumberFor<Self>>;

        /// A stable ID for a collator.
        type CollatorId: Member + Parameter;

        /// A conversion from account ID to collator ID.
        ///
        /// Its cost must be at most one storage read.
        type CollatorIdOf: Convert<Self::AccountId, Option<Self::CollatorId>>;

        /// Validate a user is registered.
        type CollatorRegistration: ValidatorRegistration<Self::CollatorId>;

        /// Minimum amount of stake an account can add to a candidate.
        type MinStake: Get<BalanceOf<Self>>;

        /// Maximum per-account number of candidates to deposit stake on.
        type MaxStakedCandidates: Get<u32>;

        /// Amount of blocks to wait before unreserving the stake by a collator.
        type CollatorUnstakingDelay: Get<BlockNumberFor<Self>>;

        /// Amount of blocks to wait before unreserving the stake by a user.
        type UserUnstakingDelay: Get<BlockNumberFor<Self>>;

        /// The weight information of this pallet.
        type WeightInfo: WeightInfo;
    }

    /// Basic information about a collation candidate.
    #[derive(
        PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
    )]
    pub struct CandidateInfo<AccountId, Balance> {
        /// Account identifier.
        pub who: AccountId,
        /// Reserved deposit.
        pub deposit: Balance,
    }

    /// Information about the unstaking requests.
    #[derive(
        PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
    )]
    pub struct UnstakeRequest<BlockNumber, Balance> {
        /// Block when stake can be unreserved.
        pub block: BlockNumber,
        /// Stake to be unreserved.
        pub amount: Balance,
    }

    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    /// The invulnerable, permissioned collators. This list must be sorted.
    #[pallet::storage]
    pub type Invulnerables<T: Config> =
        StorageValue<_, BoundedVec<T::AccountId, T::MaxInvulnerables>, ValueQuery>;

    /// The (community, limited) collation candidates. `Candidates` and `Invulnerables` should be
    /// mutually exclusive.
    ///
    /// This list is sorted in ascending order by deposit and when the deposits are equal, the least
    /// recently updated is considered greater.
    #[pallet::storage]
    pub type CandidateList<T: Config> = StorageValue<
        _,
        BoundedVec<CandidateInfo<T::AccountId, BalanceOf<T>>, T::MaxCandidates>,
        ValueQuery,
    >;

    /// Last block authored by collator.
    #[pallet::storage]
    pub type LastAuthoredBlock<T: Config> =
        StorageMap<_, Twox64Concat, T::AccountId, BlockNumberFor<T>, ValueQuery>;

    /// Desired number of candidates.
    ///
    /// This should ideally always be less than [`Config::MaxCandidates`] for weights to be correct.
    /// This should also be less than the session length, as otherwise rewards will not be able
    /// to be delivered.
    #[pallet::storage]
    pub type DesiredCandidates<T> = StorageValue<_, u32, ValueQuery>;

    /// Fixed amount to deposit to become a collator.
    ///
    /// When a collator calls `leave_intent` they immediately receive the deposit back.
    #[pallet::storage]
    pub type CandidacyBond<T> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Stores the amount staked by a given user into a candidate.
    ///
    /// First key is the candidate, and second one is the staker.
    #[pallet::storage]
    pub type Stake<T: Config> = StorageDoubleMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Blake2_128Concat,
        T::AccountId,
        BalanceOf<T>,
        ValueQuery,
    >;

    /// Unstaking requests a given user has.
    ///
    /// They can be claimed by calling the [`claim`] extrinsic.
    #[pallet::storage]
    pub type UnstakingRequests<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        BoundedVec<UnstakeRequest<BlockNumberFor<T>, BalanceOf<T>>, T::MaxStakedCandidates>,
        ValueQuery,
    >;

    /// Percentage of rewards that would go for collators.
    #[pallet::storage]
    pub type CollatorRewardPercentage<T: Config> = StorageValue<_, Percent, ValueQuery>;

    /// Per-block extra reward.
    #[pallet::storage]
    pub type ExtraReward<T: Config> = StorageValue<_, (T::AccountId, BalanceOf<T>), OptionQuery>;

    /// Blocks produced in the current session.
    #[pallet::storage]
    pub type TotalBlocks<T: Config> =
        StorageMap<_, Blake2_128Concat, SessionIndex, u32, ValueQuery>;

    /// Rewards generated for a given session.
    #[pallet::storage]
    pub type Rewards<T: Config> =
        StorageMap<_, Blake2_128Concat, SessionIndex, BalanceOf<T>, ValueQuery>;

    /// Blocks produced by each collator in a given session.
    #[pallet::storage]
    pub type ProducedBlocks<T: Config> = StorageDoubleMap<
        _,
        Blake2_128Concat,
        SessionIndex,
        Blake2_128Concat,
        T::AccountId,
        u32,
        ValueQuery,
    >;

    /// Current session index.
    #[pallet::storage]
    pub type CurrentSession<T: Config> = StorageValue<_, SessionIndex, ValueQuery>;

    /// Percentage of reward to be re-invested in collators.
    #[pallet::storage]
    pub type Autocompound<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Percent, ValueQuery>;

    #[pallet::genesis_config]
    #[derive(DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub invulnerables: Vec<T::AccountId>,
        pub candidacy_bond: BalanceOf<T>,
        pub desired_candidates: u32,
        pub candidate_reward_percentage: Percent,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            let duplicate_invulnerables = self
                .invulnerables
                .iter()
                .collect::<sp_std::collections::btree_set::BTreeSet<_>>();
            assert_eq!(
                duplicate_invulnerables.len(),
                self.invulnerables.len(),
                "duplicate invulnerables in genesis."
            );

            let mut bounded_invulnerables =
                BoundedVec::<_, T::MaxInvulnerables>::try_from(self.invulnerables.clone())
                    .expect("genesis invulnerables are more than T::MaxInvulnerables");
            assert!(
                T::MaxCandidates::get() >= self.desired_candidates,
                "genesis desired_candidates are more than T::MaxCandidates",
            );

            bounded_invulnerables.sort();

            DesiredCandidates::<T>::put(self.desired_candidates);
            CandidacyBond::<T>::put(self.candidacy_bond);
            Invulnerables::<T>::put(bounded_invulnerables);
            CollatorRewardPercentage::<T>::put(self.candidate_reward_percentage);
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub (super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// New Invulnerables were set.
        NewInvulnerables { invulnerables: Vec<T::AccountId> },
        /// A new Invulnerable was added.
        InvulnerableAdded { account_id: T::AccountId },
        /// An Invulnerable was removed.
        InvulnerableRemoved { account_id: T::AccountId },
        /// The number of desired candidates was set.
        NewDesiredCandidates { desired_candidates: u32 },
        /// The candidacy bond was set.
        NewCandidacyBond { bond_amount: BalanceOf<T> },
        /// A new candidate joined.
        CandidateAdded {
            account_id: T::AccountId,
            deposit: BalanceOf<T>,
        },
        /// A candidate was removed.
        CandidateRemoved { account_id: T::AccountId },
        /// An account was replaced in the candidate list by another one.
        CandidateReplaced {
            old: T::AccountId,
            new: T::AccountId,
            deposit: BalanceOf<T>,
        },
        /// An account was unable to be added to the Invulnerables because they did not have keys
        /// registered. Other Invulnerables may have been set.
        InvalidInvulnerableSkipped { account_id: T::AccountId },
        StakeAdded {
            staker: T::AccountId,
            candidate: T::AccountId,
            amount: BalanceOf<T>,
        },
        StakeRemoved {
            staker: T::AccountId,
            candidate: T::AccountId,
            amount: BalanceOf<T>,
        },
        StakeClaimed {
            staker: T::AccountId,
            amount: BalanceOf<T>,
        },
        UnstakeRequestCreated {
            staker: T::AccountId,
            amount: BalanceOf<T>,
            block: BlockNumberFor<T>,
        },
        StakingRewardReceived {
            staker: T::AccountId,
            amount: BalanceOf<T>,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        /// The pallet has too many candidates.
        TooManyCandidates,
        /// Leaving would result in too few candidates.
        TooFewEligibleCollators,
        /// Account is already a candidate.
        AlreadyCandidate,
        /// Account is not a candidate.
        NotCandidate,
        /// There are too many Invulnerables.
        TooManyInvulnerables,
        /// Account is already an Invulnerable.
        AlreadyInvulnerable,
        /// Account is not an Invulnerable.
        NotInvulnerable,
        /// Account has no associated validator ID.
        NoAssociatedCollatorId,
        /// Collator ID is not yet registered.
        CollatorNotRegistered,
        /// Could not insert in the candidate list.
        InsertToCandidateListFailed,
        /// Could not remove from the candidate list.
        RemoveFromCandidateListFailed,
        /// Could not update the candidate list.
        UpdateCandidateListFailed,
        /// Deposit amount is too low to take the target's slot in the candidate list.
        InsufficientBond,
        /// The target account to be replaced in the candidate list is not a candidate.
        TargetIsNotCandidate,
        /// The updated deposit amount is equal to the amount already reserved.
        IdenticalDeposit,
        /// Cannot lower candidacy bond while occupying a future collator slot in the list.
        InvalidUnreserve,
        /// Amount not sufficient to be staked.
        InsufficientStake,
        /// A collator cannot be removed during the session.
        IsCollator,
        /// DesiredCandidates is out of bounds.
        TooManyDesiredCandidates,
        /// Too many unstaking requests. Claim some of them first.
        TooManyUnstakingRequests,
        /// Cannot take some candidate's slot while the candidate list is not full.
        CanRegister,
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn integrity_test() {
            assert!(
                T::MinEligibleCollators::get() > 0,
                "chain must require at least one collator"
            );
            assert!(
                T::MaxInvulnerables::get().saturating_add(T::MaxCandidates::get())
                    >= T::MinEligibleCollators::get(),
                "invulnerables and candidates must be able to satisfy collator demand"
            );
            assert!(
                CandidacyBond::<T>::get() >= T::MinStake::get(),
                "CandidacyBond must be greater than or equal to MinStake"
            );
        }

        #[cfg(feature = "try-runtime")]
        fn try_state(_: BlockNumberFor<T>) -> Result<(), sp_runtime::TryRuntimeError> {
            Self::do_try_state()
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Set the list of invulnerable (fixed) collators. These collators must do some
        /// preparation, namely to have registered session keys.
        ///
        /// The call will remove any accounts that have not registered keys from the set. That is,
        /// it is non-atomic; the caller accepts all `AccountId`s passed in `new` _individually_ as
        /// acceptable Invulnerables, and is not proposing a _set_ of new Invulnerables.
        ///
        /// This call does not maintain mutual exclusivity of `Invulnerables` and `Candidates`. It
        /// is recommended to use a batch of `add_invulnerable` and `remove_invulnerable` instead. A
        /// `batch_all` can also be used to enforce atomicity. If any candidates are included in
        /// `new`, they should be removed with `remove_invulnerable_candidate` after execution.
        ///
        /// Must be called by the `UpdateOrigin`.
        #[pallet::call_index(0)]
        #[pallet::weight(T::WeightInfo::set_invulnerables(new.len() as u32))]
        pub fn set_invulnerables(origin: OriginFor<T>, new: Vec<T::AccountId>) -> DispatchResult {
            T::UpdateOrigin::ensure_origin(origin)?;

            // don't wipe out the collator set
            if new.is_empty() {
                // Casting `u32` to `usize` should be safe on all machines running this.
                ensure!(
                    CandidateList::<T>::decode_len().unwrap_or_default()
                        >= T::MinEligibleCollators::get() as usize,
                    Error::<T>::TooFewEligibleCollators
                );
            }

            // Will need to check the length again when putting into a bounded vec, but this
            // prevents the iterator from having too many elements.
            ensure!(
                new.len() as u32 <= T::MaxInvulnerables::get(),
                Error::<T>::TooManyInvulnerables
            );

            let mut new_with_keys = Vec::new();

            // check if the invulnerables have associated validator keys before they are set
            for account_id in &new {
                // don't let one unprepared collator ruin things for everyone.
                let validator_key = T::CollatorIdOf::convert(account_id.clone());
                match validator_key {
                    Some(key) => {
                        // key is not registered
                        if !T::CollatorRegistration::is_registered(&key) {
                            Self::deposit_event(Event::InvalidInvulnerableSkipped {
                                account_id: account_id.clone(),
                            });
                            continue;
                        }
                        // else condition passes; key is registered
                    }
                    // key does not exist
                    None => {
                        Self::deposit_event(Event::InvalidInvulnerableSkipped {
                            account_id: account_id.clone(),
                        });
                        continue;
                    }
                }

                new_with_keys.push(account_id.clone());
            }

            // should never fail since `new_with_keys` must be equal to or shorter than `new`
            let mut bounded_invulnerables =
                BoundedVec::<_, T::MaxInvulnerables>::try_from(new_with_keys)
                    .map_err(|_| Error::<T>::TooManyInvulnerables)?;

            // Invulnerables must be sorted for removal.
            bounded_invulnerables.sort();

            Invulnerables::<T>::put(&bounded_invulnerables);
            Self::deposit_event(Event::NewInvulnerables {
                invulnerables: bounded_invulnerables.to_vec(),
            });

            Ok(())
        }

        /// Set the ideal number of non-invulnerable collators. If lowering this number, then the
        /// number of running collators could be higher than this figure. Aside from that edge case,
        /// there should be no other way to have more candidates than the desired number.
        ///
        /// The origin for this call must be the `UpdateOrigin`.
        #[pallet::call_index(1)]
        #[pallet::weight(T::WeightInfo::set_desired_candidates())]
        pub fn set_desired_candidates(
            origin: OriginFor<T>,
            max: u32,
        ) -> DispatchResultWithPostInfo {
            T::UpdateOrigin::ensure_origin(origin)?;
            ensure!(
                max <= T::MaxCandidates::get() + T::MaxInvulnerables::get(),
                Error::<T>::TooManyDesiredCandidates
            );
            DesiredCandidates::<T>::put(max);
            Self::deposit_event(Event::NewDesiredCandidates {
                desired_candidates: max,
            });
            Ok(().into())
        }

        /// Set the candidacy bond amount.
        ///
        /// If the candidacy bond is increased by this call, all current candidates which have a
        /// deposit lower than the new bond will be kicked from the list and get their deposits
        /// back.
        ///
        /// The origin for this call must be the `UpdateOrigin`.
        #[pallet::call_index(2)]
        #[pallet::weight(T::WeightInfo::set_candidacy_bond(
            T::MaxCandidates::get(),
            T::MaxCandidates::get()
        ))]
        pub fn set_candidacy_bond(
            origin: OriginFor<T>,
            bond: BalanceOf<T>,
        ) -> DispatchResultWithPostInfo {
            T::UpdateOrigin::ensure_origin(origin)?;
            let bond_increased = CandidacyBond::<T>::mutate(|old_bond| -> bool {
                let bond_increased = *old_bond < bond;
                *old_bond = bond;
                bond_increased
            });
            let initial_len = CandidateList::<T>::decode_len().unwrap_or_default();
            let kicked = (bond_increased && initial_len > 0)
                .then(|| {
                    // Closure below returns the number of candidates which were kicked because
                    // their deposits were lower than the new candidacy bond.
                    CandidateList::<T>::mutate(|candidates| -> usize {
                        let first_safe_candidate = candidates
                            .iter()
                            .position(|candidate| candidate.deposit >= bond)
                            .unwrap_or(initial_len);
                        let kicked_candidates = candidates.drain(..first_safe_candidate);
                        for candidate in kicked_candidates {
                            LastAuthoredBlock::<T>::remove(candidate.who);
                        }
                        first_safe_candidate
                    })
                })
                .unwrap_or_default();
            Self::deposit_event(Event::NewCandidacyBond { bond_amount: bond });
            Ok(Some(T::WeightInfo::set_candidacy_bond(
                bond_increased
                    .then_some(initial_len as u32)
                    .unwrap_or_default(),
                kicked as u32,
            ))
            .into())
        }

        /// Register this account as a collator candidate. The account must (a) already have
        /// registered session keys and (b) be able to reserve the `CandidacyBond`.
        ///
        /// This call is not available to `Invulnerable` collators.
        #[pallet::call_index(3)]
        #[pallet::weight(T::WeightInfo::register_as_candidate(T::MaxCandidates::get()))]
        pub fn register_as_candidate(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;

            // ensure we are below limit.
            let length: u32 = CandidateList::<T>::decode_len()
                .unwrap_or_default()
                .try_into()
                .unwrap_or_default();
            ensure!(
                length < T::MaxCandidates::get(),
                Error::<T>::TooManyCandidates
            );
            ensure!(
                !Self::is_invulnerable(&who),
                Error::<T>::AlreadyInvulnerable
            );

            let validator_key =
                T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
            ensure!(
                T::CollatorRegistration::is_registered(&validator_key),
                Error::<T>::CollatorNotRegistered
            );

            Self::do_register_as_candidate(&who)?;
            // Safe to do unchecked add here because we ensure above that `length <
            // T::MaxCandidates::get()`, and since `T::MaxCandidates` is `u32` it can be at most
            // `u32::MAX`, therefore `length + 1` cannot overflow.
            Ok(Some(T::WeightInfo::register_as_candidate(length + 1)).into())
        }

        /// Deregister `origin` as a collator candidate. Note that the collator can only leave on
        /// session change.
        ///
        /// This call will fail if the total number of candidates would drop below
        /// `MinEligibleCollators`.
        #[pallet::call_index(4)]
        #[pallet::weight(T::WeightInfo::leave_intent(T::MaxCandidates::get()))]
        pub fn leave_intent(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;
            ensure!(
                Self::eligible_collators() > T::MinEligibleCollators::get(),
                Error::<T>::TooFewEligibleCollators
            );
            let length = CandidateList::<T>::decode_len().unwrap_or_default();
            // Do remove their last authored block.
            Self::try_remove_candidate_from_account(&who, true, true)?;

            Ok(Some(T::WeightInfo::leave_intent(length.saturating_sub(1) as u32)).into())
        }

        /// Add a new account `who` to the list of `Invulnerables` collators. `who` must have
        /// registered session keys. If `who` is a candidate, they will be removed.
        ///
        /// The origin for this call must be the `UpdateOrigin`.
        #[pallet::call_index(5)]
        #[pallet::weight(T::WeightInfo::add_invulnerable(
        T::MaxInvulnerables::get().saturating_sub(1),
        T::MaxCandidates::get()
        ))]
        pub fn add_invulnerable(
            origin: OriginFor<T>,
            who: T::AccountId,
        ) -> DispatchResultWithPostInfo {
            T::UpdateOrigin::ensure_origin(origin)?;

            // ensure `who` has registered a validator key
            let validator_key =
                T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
            ensure!(
                T::CollatorRegistration::is_registered(&validator_key),
                Error::<T>::CollatorNotRegistered
            );

            Invulnerables::<T>::try_mutate(|invulnerables| -> DispatchResult {
                match invulnerables.binary_search(&who) {
                    Ok(_) => return Err(Error::<T>::AlreadyInvulnerable)?,
                    Err(pos) => invulnerables
                        .try_insert(pos, who.clone())
                        .map_err(|_| Error::<T>::TooManyInvulnerables)?,
                }
                Ok(())
            })?;

            // Error just means `who` wasn't a candidate, which is the state we want anyway. Don't
            // remove their last authored block, as they are still a collator.
            let _ = Self::try_remove_candidate_from_account(&who, false, false);

            Self::deposit_event(Event::InvulnerableAdded { account_id: who });

            let weight_used = T::WeightInfo::add_invulnerable(
                Invulnerables::<T>::decode_len()
                    .unwrap_or_default()
                    .try_into()
                    .unwrap_or(T::MaxInvulnerables::get().saturating_sub(1)),
                CandidateList::<T>::decode_len()
                    .unwrap_or_default()
                    .try_into()
                    .unwrap_or(T::MaxCandidates::get()),
            );

            Ok(Some(weight_used).into())
        }

        /// Remove an account `who` from the list of `Invulnerables` collators. `Invulnerables` must
        /// be sorted.
        ///
        /// The origin for this call must be the `UpdateOrigin`.
        #[pallet::call_index(6)]
        #[pallet::weight(T::WeightInfo::remove_invulnerable(T::MaxInvulnerables::get()))]
        pub fn remove_invulnerable(origin: OriginFor<T>, who: T::AccountId) -> DispatchResult {
            T::UpdateOrigin::ensure_origin(origin)?;

            ensure!(
                Self::eligible_collators() > T::MinEligibleCollators::get(),
                Error::<T>::TooFewEligibleCollators
            );

            Invulnerables::<T>::try_mutate(|invulnerables| -> DispatchResult {
                let pos = invulnerables
                    .binary_search(&who)
                    .map_err(|_| Error::<T>::NotInvulnerable)?;
                invulnerables.remove(pos);
                Ok(())
            })?;

            Self::deposit_event(Event::InvulnerableRemoved { account_id: who });
            Ok(())
        }

        /// The caller `origin` replaces a candidate `target` in the collator candidate list by
        /// reserving `deposit`. The amount `deposit` reserved by the caller must be greater than
        /// the existing bond of the target it is trying to replace.
        ///
        /// This call will fail if the caller is already a collator candidate or invulnerable, the
        /// caller does not have registered session keys, the target is not a collator candidate,
        /// and/or the `deposit` amount cannot be reserved.
        #[pallet::call_index(7)]
        #[pallet::weight(T::WeightInfo::take_candidate_slot(T::MaxCandidates::get()))]
        pub fn take_candidate_slot(
            origin: OriginFor<T>,
            deposit: BalanceOf<T>,
            target: T::AccountId,
        ) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;

            ensure!(
                !Self::is_invulnerable(&who),
                Error::<T>::AlreadyInvulnerable
            );
            ensure!(
                deposit >= CandidacyBond::<T>::get(),
                Error::<T>::InsufficientBond
            );

            let collator_key =
                T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
            ensure!(
                T::CollatorRegistration::is_registered(&collator_key),
                Error::<T>::CollatorNotRegistered
            );

            // only allow this operation if the candidate list is full
            let length = CandidateList::<T>::decode_len().unwrap_or_default();
            ensure!(
                length >= T::MaxCandidates::get() as usize,
                Error::<T>::CanRegister
            );

            let target_info = Self::try_remove_candidate_from_account(&target, true, false)?;
            ensure!(deposit > target_info.deposit, Error::<T>::InsufficientBond);

            Self::deposit_event(Event::CandidateReplaced {
                old: target,
                new: who,
                deposit,
            });
            Ok(Some(T::WeightInfo::take_candidate_slot(length as u32)).into())
        }

        /// Adds stake to a candidate.
        ///
        /// The call will fail if:
        ///     - `origin` does not have the at least `MinStake` deposited in the candidate.
        ///     - `candidate` is not in the [`CandidateList`].
        #[pallet::call_index(8)]
        #[pallet::weight(T::WeightInfo::stake(T::MaxCandidates::get()))]
        pub fn stake(
            origin: OriginFor<T>,
            candidate: T::AccountId,
            stake: BalanceOf<T>,
        ) -> DispatchResultWithPostInfo {
            let who = ensure_signed(origin)?;
            Self::do_stake_for_account(&who, stake, &candidate, true)?;
            Ok(Some(T::WeightInfo::stake(
                CandidateList::<T>::decode_len().unwrap_or_default() as u32,
            ))
            .into())
        }

        /// Removes stake from an account.
        ///
        /// If the account is a candidate the caller will get the funds after a delay. Otherwise,
        /// funds will be returned immediately.
        #[pallet::call_index(9)]
        #[pallet::weight({0})]
        pub fn unstake_from(origin: OriginFor<T>, candidate: T::AccountId) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let (has_penalty, maybe_position) = match Self::get_candidate(&candidate) {
                Ok(pos) => (true, Some(pos)),
                Err(_) => (false, None),
            };
            Self::do_unstake(&who, &candidate, has_penalty, maybe_position)?;
            Ok(())
        }

        /// Removes all stake from all candidates.
        ///
        /// If the account was once a candidate, but it has not been unstaked, funds will be
        /// retrieved immediately.
        #[pallet::call_index(10)]
        #[pallet::weight({0})]
        pub fn unstake_all(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let candidate_set: BTreeMap<T::AccountId, usize> = CandidateList::<T>::get()
                .iter()
                .enumerate()
                .map(|(pos, c)| (c.who.clone(), pos))
                .collect();
            for (candidate, staker, stake) in Stake::<T>::iter() {
                if staker == who && !stake.is_zero() {
                    let (is_candidate, maybe_position) = match candidate_set.get(&candidate) {
                        None => (false, None),
                        Some(pos) => (true, Some(*pos)),
                    };
                    Self::do_unstake(&who, &candidate, is_candidate, maybe_position)?;
                }
            }
            Ok(())
        }

        /// Claims all pending `UnstakeRequests` for a given account.
        #[pallet::call_index(11)]
        #[pallet::weight({0})]
        pub fn claim(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            Self::do_claim(&who)
        }

        /// Sets the percentage of rewards that should be autocompounded in the same candidate.
        #[pallet::call_index(12)]
        #[pallet::weight({0})]
        pub fn set_autocompound(origin: OriginFor<T>, percent: Percent) -> DispatchResult {
            let who = ensure_signed(origin)?;
            Autocompound::<T>::insert(&who, percent);
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// Get a unique, inaccessible account ID from the `PotId`.
        pub fn account_id() -> T::AccountId {
            T::PotId::get().into_account_truncating()
        }

        /// Checks whether a given account is a candidate and returns its position if successful.
        ///
        /// Computes in **O(n)** time.
        fn get_candidate(account: &T::AccountId) -> Result<usize, ()> {
            match CandidateList::<T>::get()
                .iter()
                .position(|c| c.who == *account)
            {
                Some(pos) => Ok(pos),
                None => Err(()),
            }
        }

        /// Checks whether a given account is an invulnerable.
        ///
        /// Computes in **O(log n)** time.
        fn is_invulnerable(account: &T::AccountId) -> bool {
            Invulnerables::<T>::get().binary_search(account).is_ok()
        }

        /// Adds stake into a given candidate by providing its address.
        ///
        /// Computes in **O(n)** time.
        fn do_stake_for_account(
            staker: &T::AccountId,
            amount: BalanceOf<T>,
            candidate: &T::AccountId,
            sort: bool,
        ) -> Result<usize, DispatchError> {
            let position = Self::get_candidate(candidate).map_err(|_| Error::<T>::NotCandidate)?;
            Self::do_stake_at_position(staker, amount, position, sort)
        }

        /// Registers a given account as candidate.
        ///
        /// The account has to reserve the candidacy bond. If the account was previously a candidate
        /// the retained stake will be reincluded.
        fn do_register_as_candidate(
            who: &T::AccountId,
        ) -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
            let deposit = CandidacyBond::<T>::get();

            // In case the staker already had non-claimed stake we calculate it now.
            // TODO check the performance penalty of this operation. The collection is unbounded.
            let already_staked: BalanceOf<T> = Stake::<T>::iter_prefix_values(who)
                .fold(0u32.into(), |acc, s| acc.saturating_add(s));

            // First authored block is current block plus kick threshold to handle session delay
            CandidateList::<T>::try_mutate(
                |candidates| -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
                    ensure!(
                        !candidates
                            .iter()
                            .any(|candidate_info| candidate_info.who == *who),
                        Error::<T>::AlreadyCandidate
                    );
                    LastAuthoredBlock::<T>::insert(
                        who.clone(),
                        Self::current_block_number() + T::KickThreshold::get(),
                    );
                    let info = CandidateInfo {
                        who: who.clone(),
                        deposit: already_staked,
                    };
                    candidates
                        .try_insert(0, info.clone())
                        .map_err(|_| Error::<T>::InsertToCandidateListFailed)?;
                    // We must add the stake AFTER inserting the candidate into the list.
                    // otherwise the function will check the account is not yet a candidate and fail.
                    let sort = !already_staked.is_zero();
                    Self::do_stake_at_position(who, deposit, 0, sort)?;

                    Self::deposit_event(Event::CandidateAdded {
                        account_id: who.clone(),
                        deposit,
                    });
                    Ok(info)
                },
            )
        }

        /// Claims all pending unstaking requests for a given user.
        fn do_claim(who: &T::AccountId) -> DispatchResult {
            let mut claimed: BalanceOf<T> = 0u32.into();
            UnstakingRequests::<T>::try_mutate(who, |requests| {
                let curr_block = Self::current_block_number();
                let mut pos = 0;
                for request in requests.iter() {
                    if request.block > curr_block {
                        break;
                    }
                    pos += 1;
                    T::Currency::unreserve(who, request.amount);
                    claimed.saturating_accrue(request.amount);
                }
                requests.drain(..pos);
                Self::deposit_event(Event::StakeClaimed {
                    staker: who.clone(),
                    amount: claimed,
                });
                Ok(())
            })
        }

        /// Adds stake into a given candidate by providing its position in [`CandidatesList`].
        ///
        /// Returns the position of the candidate in the list after adding the stake.
        ///
        /// Computes in **O(1)** time.
        fn do_stake_at_position(
            staker: &T::AccountId,
            amount: BalanceOf<T>,
            position: usize,
            sort: bool,
        ) -> Result<usize, DispatchError> {
            ensure!(
                position < CandidateList::<T>::decode_len().unwrap_or_default(),
                Error::<T>::NotCandidate
            );
            let candidate = &mut CandidateList::<T>::get()[position];
            Self::add_stake_to_candidate(staker, amount, candidate)?;
            let final_position = if sort {
                Self::reassign_candidate_position(position)?
            } else {
                position
            };
            Ok(final_position)
        }

        /// Relocate a candidate after modifying its stake.
        ///
        /// Returns the final position of the candidate.
        fn reassign_candidate_position(position: usize) -> Result<usize, DispatchError> {
            CandidateList::<T>::try_mutate(|candidates| -> Result<usize, DispatchError> {
                let info = candidates.remove(position);
                let new_pos = candidates
                    .iter()
                    .position(|candidate| candidate.deposit >= info.deposit)
                    .unwrap_or_else(|| candidates.len());
                candidates
                    .try_insert(new_pos, info)
                    .map_err(|_| Error::<T>::InsertToCandidateListFailed)?;
                Ok(new_pos)
            })
        }

        /// Adds stake into a given candidate by providing its mutable reference.
        ///
        /// Returns a tuple containing the stake for the staker and the candidate after the operation
        /// takes place.
        ///
        /// Computes in **O(1)** time.
        fn add_stake_to_candidate(
            staker: &T::AccountId,
            amount: BalanceOf<T>,
            candidate: &mut CandidateInfo<T::AccountId, BalanceOf<T>>,
        ) -> Result<(BalanceOf<T>, BalanceOf<T>), DispatchError> {
            Stake::<T>::try_mutate(
                candidate.who.clone(),
                staker,
                |stake| -> Result<(BalanceOf<T>, BalanceOf<T>), DispatchError> {
                    let final_staker_stake = stake.saturating_add(amount);
                    ensure!(
                        final_staker_stake >= T::MinStake::get(),
                        Error::<T>::InsufficientStake
                    );
                    T::Currency::reserve(staker, amount)?;
                    *stake = final_staker_stake;
                    candidate.deposit.saturating_accrue(amount);
                    Self::deposit_event(Event::StakeAdded {
                        staker: staker.clone(),
                        candidate: candidate.who.clone(),
                        amount,
                    });
                    Ok((final_staker_stake, candidate.deposit))
                },
            )
        }

        /// Return the total number of accounts that are eligible collators (candidates and
        /// invulnerables).
        fn eligible_collators() -> u32 {
            CandidateList::<T>::decode_len()
                .unwrap_or_default()
                .saturating_add(Invulnerables::<T>::decode_len().unwrap_or_default())
                .try_into()
                .unwrap_or(u32::MAX)
        }

        /// Unstakes all funds deposited in a given `candidate`.
        ///
        /// If the target is not a candidate or if the operation does not carry a penalty the deposit
        /// is immediately returned. Otherwise, a delay is applied.
        ///
        /// Returns the amount unstaked.
        ///
        /// Computes in **O(1)**.
        fn do_unstake(
            staker: &T::AccountId,
            candidate: &T::AccountId,
            has_penalty: bool,
            maybe_position: Option<usize>,
        ) -> Result<BalanceOf<T>, DispatchError> {
            let stake = Stake::<T>::take(candidate, staker);
            if !stake.is_zero() {
                if !has_penalty {
                    T::Currency::unreserve(staker, stake);
                } else {
                    let delay = if staker == candidate {
                        T::CollatorUnstakingDelay::get()
                    } else {
                        T::UserUnstakingDelay::get()
                    };
                    UnstakingRequests::<T>::try_mutate(staker, |requests| -> DispatchResult {
                        let block = Self::current_block_number() + delay;
                        requests
                            .try_push(UnstakeRequest {
                                block,
                                amount: stake,
                            })
                            .map_err(|_| Error::<T>::TooManyUnstakingRequests)?;
                        Self::deposit_event(Event::UnstakeRequestCreated {
                            staker: staker.clone(),
                            amount: stake,
                            block,
                        });
                        Ok(())
                    })?;
                }
                if let Some(position) = maybe_position {
                    Self::reassign_candidate_position(position)?;
                }
            }
            Ok(stake)
        }

        /// Removes a candidate, identified by its index, if it exists and refunds the stake.
        ///
        /// Returns the candidate info before its removal.
        fn try_remove_candidate_at_position(
            idx: usize,
            remove_last_authored: bool,
            has_penalty: bool,
        ) -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
            CandidateList::<T>::try_mutate(
                |candidates| -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
                    let candidate = candidates.remove(idx);
                    if remove_last_authored {
                        LastAuthoredBlock::<T>::remove(candidate.who.clone())
                    };
                    Self::do_unstake(&candidate.who, &candidate.who, has_penalty, None)?;
                    Self::deposit_event(Event::CandidateRemoved {
                        account_id: candidate.who.clone(),
                    });
                    Ok(candidate)
                },
            )
        }

        /// Removes a candidate, identified by its account, if it exists and refunds the stake.
        ///
        /// Returns the candidate info before its removal.
        fn try_remove_candidate_from_account(
            who: &T::AccountId,
            remove_last_authored: bool,
            has_penalty: bool,
        ) -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
            let idx = CandidateList::<T>::get()
                .iter()
                .position(|candidate_info| candidate_info.who == *who)
                .ok_or(Error::<T>::NotCandidate)?;
            Self::try_remove_candidate_at_position(idx, remove_last_authored, has_penalty)
        }

        /// Distributes the rewards associated for a given collator, obtained during the previous session.
        /// This includes specific rewards for the collator plus rewards for the stakers.
        ///
        /// The collator must be a candidate in order to receive the rewards.
        fn do_reward_collator(collator: &T::AccountId, blocks: u32, session: SessionIndex) {
            if let Ok(pos) = Self::get_candidate(collator) {
                let collator_info = &CandidateList::<T>::get()[pos];
                let total_rewards = Rewards::<T>::get(session);
                let total_session_blocks = TotalBlocks::<T>::get(session);
                let collator_percentage = CollatorRewardPercentage::<T>::get();

                let rewards_all: BalanceOf<T> =
                    total_rewards.saturating_mul(blocks.into()) / total_session_blocks.into();
                let collator_only_reward = collator_percentage * rewards_all;

                // Reward collator. Note these rewards are not autocompounded.
                if let Err(error) = Self::do_reward_single(collator, collator_only_reward) {
                    log::warn!("Failure rewarding collator {}: {:?}", collator, error);
                }

                // Reward stakers
                let stakers_only_rewards = total_rewards.saturating_sub(collator_only_reward);
                Stake::<T>::iter_prefix(collator).for_each(|(staker, stake)| {
                    let staker_reward: BalanceOf<T> =
                        stakers_only_rewards.saturating_mul(stake) / collator_info.deposit;
                    if let Err(error) = Self::do_reward_single(&staker, staker_reward) {
                        log::warn!("Failure rewarding staker {}: {:?}", staker, error);
                    } else {
                        // Autocompound
                        let compound_percentage = Autocompound::<T>::get(staker.clone());
                        let compound_amount = compound_percentage * stake;
                        if !compound_amount.is_zero() {
                            if let Err(error) =
                                Self::do_stake_at_position(&staker, compound_amount, pos, false)
                            {
                                log::warn!(
                                    "Failure autocompounding for staker {} to candidate {}: {:?}",
                                    staker,
                                    collator,
                                    error
                                );
                            }
                        }
                    }
                });
                let _ = Self::reassign_candidate_position(pos);
            }
        }

        fn do_reward_single(who: &T::AccountId, reward: BalanceOf<T>) -> DispatchResult {
            T::Currency::transfer(&Self::account_id(), who, reward, KeepAlive)?;
            Self::deposit_event(Event::StakingRewardReceived {
                staker: who.clone(),
                amount: reward,
            });
            Ok(())
        }

        /// Gets the current block number
        pub fn current_block_number() -> BlockNumberFor<T> {
            frame_system::Pallet::<T>::block_number()
        }

        /// Assemble the current set of candidates and invulnerables into the next collator set.
        ///
        /// This is done on the fly, as frequent as we are told to do so, as the session manager.
        pub fn assemble_collators() -> Vec<T::AccountId> {
            // Casting `u32` to `usize` should be safe on all machines running this.
            let desired_candidates = DesiredCandidates::<T>::get() as usize;
            let mut collators = Invulnerables::<T>::get().to_vec();
            collators.extend(
                CandidateList::<T>::get()
                    .iter()
                    .rev()
                    .take(desired_candidates)
                    .cloned()
                    .map(|candidate_info| candidate_info.who),
            );
            collators
        }

        /// Kicks out candidates that did not produce a block in the kick threshold and refunds
        /// all their stake.
        ///
        /// Return value is the number of candidates left in the list.
        pub fn kick_stale_candidates(candidates: impl IntoIterator<Item = T::AccountId>) -> u32 {
            let now = Self::current_block_number();
            let kick_threshold = T::KickThreshold::get();
            let min_collators = T::MinEligibleCollators::get();
            candidates
                .into_iter()
                .enumerate()
                .filter_map(|(pos, c)| {
                    let last_block = LastAuthoredBlock::<T>::get(c.clone());
                    let since_last = now.saturating_sub(last_block);

                    let is_invulnerable = Self::is_invulnerable(&c);
                    let is_lazy = since_last >= kick_threshold;

                    if is_invulnerable {
                        // They are invulnerable. No reason for them to be in `CandidateList` also.
                        // We don't even care about the min collators here, because an Account
                        // should not be a collator twice.
                        let _ = Self::try_remove_candidate_at_position(pos, false, false);
                        None
                    } else if Self::eligible_collators() <= min_collators || !is_lazy {
                            // Either this is a good collator (not lazy) or we are at the minimum
                            // that the system needs. They get to stay.
                            Some(c)
                    } else {
                        // This collator has not produced a block recently enough. Bye bye.
                        // TODO check if the collator should have a penalty in this case
                        let _ = Self::try_remove_candidate_at_position(pos, true, false);
                        None
                    }
                })
                .count()
                .try_into()
                .expect("filter_map operation can't result in a bounded vec larger than its original; qed")
        }

        /// Rewards a pending collator from the previous round, if any.
        fn reward_one_collator(current_session: SessionIndex) {
            if current_session > 0 {
                let previous_session = current_session - 1;
                ProducedBlocks::<T>::iter_prefix(previous_session)
                    .drain()
                    .take(1)
                    .for_each(|(collator, blocks)| {
                        Self::do_reward_collator(&collator, blocks, previous_session);
                    });
            }
        }

        /// Ensure the correctness of the state of this pallet.
        ///
        /// This should be valid before or after each state transition of this pallet.
        ///
        /// # Invariants
        ///
        /// ## `DesiredCandidates`
        ///
        /// * The current desired candidate count should not exceed the candidate list capacity.
        /// * The number of selected candidates together with the invulnerables must be greater than
        ///   or equal to the minimum number of eligible collators.
        #[cfg(any(test, feature = "try-runtime"))]
        pub fn do_try_state() -> Result<(), sp_runtime::TryRuntimeError> {
            let desired_candidates = DesiredCandidates::<T>::get();

            frame_support::ensure!(
                desired_candidates <= T::MaxCandidates::get(),
                "Shouldn't demand more candidates than the pallet config allows."
            );

            frame_support::ensure!(
				desired_candidates.saturating_add(T::MaxInvulnerables::get()) >=
					T::MinEligibleCollators::get(),
				"Invulnerable set together with desired candidates should be able to meet the collator quota."
			);

            Ok(())
        }
    }

    /// Keep track of number of authored blocks per authority. Uncles are counted as well since
    /// they're a valid proof of being online.
    impl<T: Config + pallet_authorship::Config>
        pallet_authorship::EventHandler<T::AccountId, BlockNumberFor<T>> for Pallet<T>
    {
        fn note_author(author: T::AccountId) {
            let current_session = CurrentSession::<T>::get();
            LastAuthoredBlock::<T>::insert(author.clone(), Self::current_block_number());
            TotalBlocks::<T>::mutate(current_session, |total| total.saturating_inc());

            if !Self::is_invulnerable(&author) {
                // Invulnerables do not get rewards
                ProducedBlocks::<T>::mutate(current_session, author, |b| b.saturating_inc());
            }

            // Reward one collator
            Self::reward_one_collator(current_session);

            frame_system::Pallet::<T>::register_extra_weight_unchecked(
                T::WeightInfo::note_author(),
                DispatchClass::Mandatory,
            );
        }
    }

    /// Play the role of the session manager.
    impl<T: Config> SessionManager<T::AccountId> for Pallet<T> {
        fn new_session(index: SessionIndex) -> Option<Vec<T::AccountId>> {
            log::info!(
                "assembling new collators for new session {} at #{:?}",
                index,
                <frame_system::Pallet<T>>::block_number(),
            );

            // The `expect` below is safe because the list is a `BoundedVec` with a max size of
            // `T::MaxCandidates`, which is a `u32`. When `decode_len` returns `Some(len)`, `len`
            // must be valid and at most `u32::MAX`, which must always be able to convert to `u32`.
            let candidates_len_before: u32 = CandidateList::<T>::decode_len()
                .unwrap_or_default()
                .try_into()
                .expect("length is at most `T::MaxCandidates`, so it must fit in `u32`; qed");
            let active_candidates_count = Self::kick_stale_candidates(
                CandidateList::<T>::get()
                    .iter()
                    .map(|candidate_info| candidate_info.who.clone()),
            );
            let removed = candidates_len_before.saturating_sub(active_candidates_count);
            let result = Self::assemble_collators();

            frame_system::Pallet::<T>::register_extra_weight_unchecked(
                T::WeightInfo::new_session(candidates_len_before, removed),
                DispatchClass::Mandatory,
            );
            Some(result)
        }

        fn start_session(session: SessionIndex) {
            // Initialize counters for this session
            TotalBlocks::<T>::insert(session, 0);
            CurrentSession::<T>::put(session);

            // cleanup last session's stuff
            if session > 1 {
                let last_session = session - 2;
                TotalBlocks::<T>::remove(last_session);
                Rewards::<T>::remove(last_session);
                let _ = ProducedBlocks::<T>::clear_prefix(last_session, u32::MAX, None);
            }
        }

        fn end_session(session: SessionIndex) {
            let pot_account = Self::account_id();
            let produced_blocks = TotalBlocks::<T>::get(session);

            // Transfer the extra reward, if any, to the pot
            if let Some((account, per_block_extra_reward)) = ExtraReward::<T>::get() {
                let extra_reward = per_block_extra_reward.saturating_mul(produced_blocks.into());
                if let Err(error) =
                    T::Currency::transfer(&account, &pot_account, extra_reward, KeepAlive)
                {
                    log::warn!("Failure transferring extra rewards to the pallet-collator-staking pot account: {:?}", error);
                }
            }

            // Rewards are the total amount in the pot
            let total_rewards = T::Currency::free_balance(&pot_account);
            Rewards::<T>::insert(session, total_rewards);
        }
    }
}
