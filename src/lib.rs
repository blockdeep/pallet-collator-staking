//! Collator Staking pallet.
//!
//! A simple DPoS pallet for collators in a parachain.
//!
//! ## Overview
//!
//! The Collator Staking pallet provides DPoS functionality to manage collators of a parachain.
//! It allows stakers to stake their tokens to back collators, and receive rewards proportionately.
//! There is no slashing in place. If a collator does not produce blocks as expected,
//! they are removed from the collator set.

#![cfg_attr(not(feature = "std"), no_std)]

use core::marker::PhantomData;
use frame_support::traits::TypedGet;
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
	use super::LOG_TARGET;
	pub use crate::weights::WeightInfo;
	use frame_support::{
		dispatch::{DispatchClass, DispatchResultWithPostInfo},
		pallet_prelude::*,
		traits::{
			fungible::{Inspect, Mutate, MutateHold},
			tokens::Precision::Exact,
			tokens::Preservation::{Expendable, Preserve},
			EnsureOrigin, ValidatorRegistration,
		},
		BoundedVec, DefaultNoBound, PalletId,
	};
	use frame_system::pallet_prelude::*;
	use pallet_session::SessionManager;
	use sp_runtime::{
		traits::{AccountIdConversion, Convert, Saturating, Zero},
		RuntimeDebug,
	};
	use sp_runtime::{Perbill, Percent};
	use sp_staking::SessionIndex;
	use sp_std::collections::btree_map::BTreeMap;
	use sp_std::vec::Vec;

	/// The in-code storage version.
	const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

	/// A convertor from collators id. Since this pallet does not have stash/controller, this is
	/// just identity.
	pub struct IdentityCollator;

	impl<T> sp_runtime::traits::Convert<T, Option<T>> for IdentityCollator {
		fn convert(t: T) -> Option<T> {
			Some(t)
		}
	}

	pub struct MaxDesiredCandidates<T>(PhantomData<T>);
	impl<T: Config> TypedGet for MaxDesiredCandidates<T> {
		type Type = u32;
		fn get() -> Self::Type {
			T::MaxCandidates::get().saturating_add(T::MaxInvulnerables::get())
		}
	}

	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// Overarching event type.
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		/// The currency mechanism.
		type Currency: Inspect<Self::AccountId>
			+ Mutate<Self::AccountId>
			+ MutateHold<Self::AccountId, Reason = Self::RuntimeHoldReason>;

		/// Overarching hold reason.
		type RuntimeHoldReason: From<HoldReason>;

		/// Origin that can dictate updating parameters of this pallet.
		type UpdateOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		/// Account Identifier from which the internal Pot is generated.
		///
		/// To initiate rewards, an ED needs to be transferred to the pot address.
		type PotId: Get<PalletId>;

		/// Account Identifier from which the extra reward Pot is generated.
		///
		/// To initiate extra rewards the [`set_extra_reward`] extrinsic must be called.
		type ExtraRewardPotId: Get<PalletId>;

		/// Maximum number of candidates that we should have.
		///
		/// This does not take into account the invulnerables.
		type MaxCandidates: Get<u32>;

		/// Minimum number eligible collators. Should always be greater than zero. This includes
		/// Invulnerable collators. This ensures that there will always be one collator who can
		/// produce a block.
		#[pallet::constant]
		type MinEligibleCollators: Get<u32>;

		/// Maximum number of invulnerables.
		#[pallet::constant]
		type MaxInvulnerables: Get<u32>;

		// Will be kicked if block is not produced in threshold.
		#[pallet::constant]
		type KickThreshold: Get<BlockNumberFor<Self>>;

		/// A stable ID for a collator.
		type CollatorId: Member + Parameter;

		/// A conversion from account ID to collator ID.
		///
		/// Its cost must be at most one storage read.
		type CollatorIdOf: Convert<Self::AccountId, Option<Self::CollatorId>>;

		/// Validate a user is registered.
		type CollatorRegistration: ValidatorRegistration<Self::CollatorId>;

		/// Maximum per-account number of candidates to deposit stake on.
		#[pallet::constant]
		type MaxStakedCandidates: Get<u32>;

		/// Maximum per-candidate number of stakers.
		#[pallet::constant]
		type MaxStakers: Get<u32>;

		/// Number of blocks to wait before unreserving the stake by a collator.
		#[pallet::constant]
		type CollatorUnstakingDelay: Get<BlockNumberFor<Self>>;

		/// Number of blocks to wait before unreserving the stake by a user.
		#[pallet::constant]
		type UserUnstakingDelay: Get<BlockNumberFor<Self>>;

		/// The weight information of this pallet.
		type WeightInfo: WeightInfo;
	}

	/// A reason for the pallet placing a hold on funds.
	#[pallet::composite_enum]
	pub enum HoldReason {
		/// Funds are held for candidacy bonds and staking.
		Staking,
	}

	/// Basic information about a collator candidate.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct CandidateInfo<AccountId, Balance> {
		/// Account identifier.
		pub who: AccountId,
		/// Total stake.
		pub stake: Balance,
		/// Initial bond.
		pub deposit: Balance,
		/// Amount of stakers.
		pub stakers: u32,
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

	/// Fixed amount to stake to become a collator.
	#[pallet::storage]
	pub type CandidacyBond<T> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Minimum amount of stake an account can add to a candidate.
	#[pallet::storage]
	pub type MinStake<T> = StorageValue<_, BalanceOf<T>, ValueQuery>;

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

	/// Stores the number of candidates a given account deposited stake on.
	///
	/// Cannot be higher than `MaxStakedCandidates`.
	#[pallet::storage]
	pub type StakeCount<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, u32, ValueQuery>;

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
	pub type ExtraReward<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Candidates with pending stake to be redeemed to their stakers. Insertion and deletions
	/// are made in a FIFO manner.
	#[pallet::storage]
	pub type PendingExCandidates<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, bool, ValueQuery>;

	/// Blocks produced in the current session. First value is actual total, and second is those
	/// that have not been produced by invulnerables.
	#[pallet::storage]
	pub type TotalBlocks<T: Config> =
		StorageMap<_, Blake2_128Concat, SessionIndex, (u32, u32), ValueQuery>;

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
	pub type AutoCompound<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, Percent, ValueQuery>;

	#[pallet::genesis_config]
	#[derive(DefaultNoBound)]
	pub struct GenesisConfig<T: Config> {
		pub invulnerables: Vec<T::AccountId>,
		pub candidacy_bond: BalanceOf<T>,
		pub min_stake: BalanceOf<T>,
		pub desired_candidates: u32,
		pub collator_reward_percentage: Percent,
		pub extra_reward: BalanceOf<T>,
	}

	#[pallet::genesis_build]
	impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
		fn build(&self) {
			assert!(
				self.min_stake <= self.candidacy_bond,
				"min_stake is higher than candidacy_bond",
			);
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
			MinStake::<T>::put(self.min_stake);
			Invulnerables::<T>::put(bounded_invulnerables);
			CollatorRewardPercentage::<T>::put(self.collator_reward_percentage);
			ExtraReward::<T>::put(self.extra_reward);
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
		CandidateAdded { account_id: T::AccountId, deposit: BalanceOf<T> },
		/// A candidate was removed.
		CandidateRemoved { account_id: T::AccountId },
		/// An account was replaced in the candidate list by another one.
		CandidateReplaced {
			old: T::AccountId,
			new: T::AccountId,
			deposit: BalanceOf<T>,
			stake: BalanceOf<T>,
		},
		/// An account was unable to be added to the Invulnerables because they did not have keys
		/// registered. Other Invulnerables may have been set.
		InvalidInvulnerableSkipped { account_id: T::AccountId },
		/// A staker added stake to a candidate.
		StakeAdded { staker: T::AccountId, candidate: T::AccountId, amount: BalanceOf<T> },
		/// Stake was claimed after a penalty period.
		StakeClaimed { staker: T::AccountId, amount: BalanceOf<T> },
		/// An unstake request was created.
		UnstakeRequestCreated {
			staker: T::AccountId,
			candidate: T::AccountId,
			amount: BalanceOf<T>,
			block: BlockNumberFor<T>,
		},
		/// A staker removed stake from a candidate
		StakeRemoved { staker: T::AccountId, candidate: T::AccountId, amount: BalanceOf<T> },
		/// A staking reward was delivered.
		StakingRewardReceived { staker: T::AccountId, amount: BalanceOf<T> },
		/// AutoCompound percentage was set.
		AutoCompoundPercentageSet { staker: T::AccountId, percentage: Percent },
		/// Collator reward percentage was set.
		CollatorRewardPercentageSet { percentage: Percent },
		/// The extra reward was set.
		ExtraRewardSet { amount: BalanceOf<T> },
		/// The extra reward was removed.
		ExtraRewardRemoved {},
		/// The minimum amount to stake was changed.
		NewMinStake { min_stake: BalanceOf<T> },
		/// A session just ended.
		SessionEnded { index: SessionIndex, rewards: BalanceOf<T> },
		/// The extra reward pot account was funded.
		ExtraRewardPotFunded { pot: T::AccountId, amount: BalanceOf<T> },
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
		/// Deposit amount is too low to take the target's slot in the candidate list.
		InsufficientDeposit,
		/// Amount not sufficient to be staked.
		InsufficientStake,
		/// DesiredCandidates is out of bounds.
		TooManyDesiredCandidates,
		/// Too many unstaking requests. Claim some of them first.
		TooManyUnstakingRequests,
		/// Cannot take some candidate's slot while the candidate list is not full.
		CanRegister,
		/// Invalid value for MinStake. It must be lower than or equal to `CandidacyBond`.
		InvalidMinStake,
		/// Invalid value for CandidacyBond. It must be higher than or equal to `MinStake`.
		InvalidCandidacyBond,
		/// Number of staked candidates is greater than `MaxStakedCandidates`.
		TooManyStakedCandidates,
		/// Extra reward cannot be zero.
		InvalidExtraReward,
		/// Extra rewards are already zero.
		ExtraRewardAlreadyDisabled,
		/// The amount to fund the extra reward pot must be greater than zero.
		InvalidFundingAmount,
		/// There is nothing to unstake.
		NothingToUnstake,
		/// Cannot add more stakers to a given candidate.
		TooManyStakers,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn integrity_test() {
			assert!(T::MinEligibleCollators::get() > 0, "chain must require at least one collator");
			assert!(
				MaxDesiredCandidates::<T>::get() >= T::MinEligibleCollators::get(),
				"invulnerables and candidates must be able to satisfy collator demand"
			);
			assert!(
				CandidacyBond::<T>::get() >= MinStake::<T>::get(),
				"CandidacyBond must be greater than or equal to MinStake"
			);
			assert!(
				T::MaxCandidates::get() >= T::MaxStakedCandidates::get(),
				"MaxCandidates must be greater than or equal to MaxStakedCandidates"
			);
		}

		/// Rewards are delivered at the beginning of each block. The underlined assumption is that
		/// the number of collators to be rewarded is much lower than the number of blocks in
		/// a given session.
		///
		/// Please note that only one collator and its stakers are rewarded per block, until all
		/// collators (and their stakers) are rewarded for the previous session.
		fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
			let mut weight = T::DbWeight::get().reads_writes(1, 0);
			let current_session = CurrentSession::<T>::get();
			if current_session > 0 {
				let (rewarded_stakers, compounded_stakers) =
					Self::reward_one_collator(current_session - 1);
				if !rewarded_stakers.is_zero() {
					weight = weight.saturating_add(T::WeightInfo::reward_one_collator(
						CandidateList::<T>::decode_len().unwrap_or_default() as u32,
						rewarded_stakers,
						compounded_stakers * 100 / rewarded_stakers,
					));
				}
			}

			weight
		}

		/// Traverses pending ex-candidates and rewards their stakers.
		///
		/// Note only at most one ex-candidate will be processed per block.
		fn on_idle(_n: BlockNumberFor<T>, remaining_weight: Weight) -> Weight {
			let mut weight = T::DbWeight::get().reads_writes(1, 0);
			let worst_case_weight = weight.saturating_add(T::WeightInfo::refund_stakers(
				T::MaxStakers::get().saturating_sub(1),
			));
			if worst_case_weight.any_gt(remaining_weight) {
				return Weight::zero();
			}
			if let Some((account, is_excandidate)) = PendingExCandidates::<T>::iter().drain().next()
			{
				weight.saturating_accrue(T::DbWeight::get().reads_writes(0, 1));

				// This must always be true. If not we simply do nothing and cleanup the storage.
				if is_excandidate {
					let stakers = Self::refund_stakers(&account);
					weight.saturating_accrue(T::WeightInfo::refund_stakers(stakers));
				}
			}
			weight
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
					},
					// key does not exist
					None => {
						Self::deposit_event(Event::InvalidInvulnerableSkipped {
							account_id: account_id.clone(),
						});
						continue;
					},
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

		/// Set the ideal number of collators. If lowering this number, then the
		/// number of running collators could be higher than this figure. Aside from that edge case,
		/// there should be no other way to have more candidates than the desired number.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(1)]
		#[pallet::weight(T::WeightInfo::set_desired_candidates())]
		pub fn set_desired_candidates(origin: OriginFor<T>, max: u32) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			ensure!(max <= MaxDesiredCandidates::<T>::get(), Error::<T>::TooManyDesiredCandidates);
			DesiredCandidates::<T>::put(max);
			Self::deposit_event(Event::NewDesiredCandidates { desired_candidates: max });
			Ok(())
		}

		/// Set the candidacy bond amount.
		///
		/// If the candidacy bond is increased by this call, all current candidates which have a
		/// deposit lower than the new bond will be kicked once the current session ends.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::set_candidacy_bond())]
		pub fn set_candidacy_bond(origin: OriginFor<T>, bond: BalanceOf<T>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			ensure!(bond >= MinStake::<T>::get(), Error::<T>::InvalidCandidacyBond);
			CandidacyBond::<T>::put(bond);
			Self::deposit_event(Event::NewCandidacyBond { bond_amount: bond });
			Ok(())
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
			ensure!(length < T::MaxCandidates::get(), Error::<T>::TooManyCandidates);
			ensure!(!Self::is_invulnerable(&who), Error::<T>::AlreadyInvulnerable);

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

		/// Deregister `origin` as a collator candidate. No rewards will be delivered to this
		/// candidate after this moment.
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
				let pos =
					invulnerables.binary_search(&who).map_err(|_| Error::<T>::NotInvulnerable)?;
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
		#[pallet::weight(T::WeightInfo::take_candidate_slot())]
		pub fn take_candidate_slot(
			origin: OriginFor<T>,
			stake: BalanceOf<T>,
			target: T::AccountId,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;

			ensure!(!Self::is_invulnerable(&who), Error::<T>::AlreadyInvulnerable);
			ensure!(Self::get_candidate(&who).is_err(), Error::<T>::AlreadyCandidate);

			let collator_key =
				T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
			ensure!(
				T::CollatorRegistration::is_registered(&collator_key),
				Error::<T>::CollatorNotRegistered
			);

			// only allow this operation if the candidate list is full
			let length = CandidateList::<T>::decode_len().unwrap_or_default();
			ensure!(length == T::MaxCandidates::get() as usize, Error::<T>::CanRegister);

			// Remove old candidate
			let target_info = Self::try_remove_candidate_from_account(&target, true, false)?;
			ensure!(stake > target_info.stake, Error::<T>::InsufficientDeposit);

			// Register the new candidate
			let candidate = Self::do_register_as_candidate(&who)?;
			Self::do_stake_at_position(&who, stake, 0, true)?;

			Self::deposit_event(Event::CandidateReplaced {
				old: target,
				new: who,
				deposit: candidate.deposit,
				stake,
			});
			Ok(())
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
			Self::do_stake_for_account(&who, &candidate, stake, true)?;
			Ok(Some(T::WeightInfo::stake(
				CandidateList::<T>::decode_len().unwrap_or_default() as u32
			))
			.into())
		}

		/// Removes stake from an account.
		///
		/// If the account is a candidate the caller will get the funds after a delay. Otherwise,
		/// funds will be returned immediately.
		///
		/// The candidate will have its position in the [`CandidateList`] conveniently modified, and
		/// if the amount of stake is below the [`CandidacyBond`] it will be kicked when the session ends.
		#[pallet::call_index(9)]
		#[pallet::weight(T::WeightInfo::unstake_from(T::MaxCandidates::get(), T::MaxStakedCandidates::get().saturating_sub(1)))]
		pub fn unstake_from(
			origin: OriginFor<T>,
			candidate: T::AccountId,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let (has_penalty, maybe_position) = match Self::get_candidate(&candidate) {
				Ok(pos) => (true, Some(pos)),
				Err(_) => (false, None),
			};
			let (_, unstaking_requests) =
				Self::do_unstake(&who, &candidate, has_penalty, maybe_position, true)?;
			Ok(Some(T::WeightInfo::unstake_from(
				CandidateList::<T>::decode_len().unwrap_or_default() as u32,
				unstaking_requests,
			))
			.into())
		}

		/// Removes all stake from all candidates.
		///
		/// If the account was once a candidate, but it has not been unstaked, funds will be
		/// retrieved immediately.
		#[pallet::call_index(10)]
		#[pallet::weight(T::WeightInfo::unstake_all(
			T::MaxCandidates::get(),
			T::MaxStakedCandidates::get()
		))]
		pub fn unstake_all(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let candidate_map: BTreeMap<T::AccountId, usize> = CandidateList::<T>::get()
				.iter()
				.enumerate()
				.map(|(pos, c)| (c.who.clone(), pos))
				.collect();
			let mut operations = 0;
			for (candidate, staker, stake) in Stake::<T>::iter() {
				if staker == who && !stake.is_zero() {
					let (is_candidate, maybe_position) = match candidate_map.get(&candidate) {
						None => (false, None),
						Some(pos) => (true, Some(*pos)),
					};
					Self::do_unstake(&who, &candidate, is_candidate, maybe_position, false)?;
					operations += 1;
				}
			}
			CandidateList::<T>::mutate(|candidates| candidates.sort_by_key(|c| c.stake));
			Ok(Some(T::WeightInfo::unstake_all(
				CandidateList::<T>::decode_len().unwrap_or_default() as u32,
				operations,
			))
			.into())
		}

		/// Claims all pending [`UnstakeRequest`] for a given account.
		#[pallet::call_index(11)]
		#[pallet::weight(T::WeightInfo::claim(T::MaxStakedCandidates::get()))]
		pub fn claim(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let operations = Self::do_claim(&who)?;
			Ok(Some(T::WeightInfo::claim(operations)).into())
		}

		/// Sets the percentage of rewards that should be autocompounded in the same candidate.
		#[pallet::call_index(12)]
		#[pallet::weight(T::WeightInfo::set_autocompound_percentage())]
		pub fn set_autocompound_percentage(
			origin: OriginFor<T>,
			percent: Percent,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			if percent.is_zero() {
				AutoCompound::<T>::remove(&who);
			} else {
				AutoCompound::<T>::insert(&who, percent);
			}
			Self::deposit_event(Event::AutoCompoundPercentageSet {
				staker: who,
				percentage: percent,
			});
			Ok(())
		}

		/// Sets the percentage of rewards that collators will have for producing blocks.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(13)]
		#[pallet::weight(T::WeightInfo::set_collator_reward_percentage())]
		pub fn set_collator_reward_percentage(
			origin: OriginFor<T>,
			percent: Percent,
		) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;

			CollatorRewardPercentage::<T>::put(percent);
			Self::deposit_event(Event::CollatorRewardPercentageSet { percentage: percent });
			Ok(())
		}

		/// Sets the extra rewards for producing blocks. Once the session finishes, the provided amount times
		/// the total number of blocks produced during the session will be transferred from the given account
		/// to the pallet's pot account to be distributed as rewards.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(14)]
		#[pallet::weight(T::WeightInfo::set_extra_reward())]
		pub fn set_extra_reward(
			origin: OriginFor<T>,
			extra_reward: BalanceOf<T>,
		) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			ensure!(!extra_reward.is_zero(), Error::<T>::InvalidExtraReward);

			ExtraReward::<T>::put(extra_reward);
			Self::deposit_event(Event::ExtraRewardSet { amount: extra_reward });
			Ok(())
		}

		/// Sets minimum amount that can be staked. The new amount must be lower than or equal to
		/// the candidacy bond.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(15)]
		#[pallet::weight(T::WeightInfo::set_minimum_stake())]
		pub fn set_minimum_stake(
			origin: OriginFor<T>,
			new_min_stake: BalanceOf<T>,
		) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			ensure!(new_min_stake <= CandidacyBond::<T>::get(), Error::<T>::InvalidMinStake);

			MinStake::<T>::put(new_min_stake);
			Self::deposit_event(Event::NewMinStake { min_stake: new_min_stake });
			Ok(())
		}

		/// Sets minimum amount that can be staked. The new amount must be lower than or equal to
		/// the candidacy bond.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(16)]
		#[pallet::weight(T::WeightInfo::stop_extra_reward())]
		pub fn stop_extra_reward(origin: OriginFor<T>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;

			let extra_reward = ExtraReward::<T>::get();
			ensure!(!extra_reward.is_zero(), Error::<T>::ExtraRewardAlreadyDisabled);

			ExtraReward::<T>::kill();
			Self::deposit_event(Event::ExtraRewardRemoved {});
			Ok(())
		}

		/// Funds the extra reward pot account.
		#[pallet::call_index(17)]
		#[pallet::weight(T::WeightInfo::top_up_extra_rewards())]
		pub fn top_up_extra_rewards(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;

			ensure!(!amount.is_zero(), Error::<T>::InvalidFundingAmount);

			let extra_reward_pot_account = Self::extra_reward_account_id();
			T::Currency::transfer(&who, &extra_reward_pot_account, amount, Preserve)?;
			Self::deposit_event(Event::<T>::ExtraRewardPotFunded {
				amount,
				pot: extra_reward_pot_account,
			});
			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {
		/// Get a unique, inaccessible account ID from the `PotId`.
		pub fn account_id() -> T::AccountId {
			T::PotId::get().into_account_truncating()
		}

		/// Get a unique, inaccessible account ID from the `PotId`.
		pub fn extra_reward_account_id() -> T::AccountId {
			T::ExtraRewardPotId::get().into_account_truncating()
		}

		/// Checks whether a given account is a candidate and returns its position if successful.
		pub fn get_candidate(account: &T::AccountId) -> Result<usize, DispatchError> {
			match CandidateList::<T>::get().iter().position(|c| c.who == *account) {
				Some(pos) => Ok(pos),
				None => Err(Error::<T>::NotCandidate.into()),
			}
		}

		/// Checks whether a given account is an invulnerable.
		pub fn is_invulnerable(account: &T::AccountId) -> bool {
			Invulnerables::<T>::get().binary_search(account).is_ok()
		}

		/// Adds stake into a given candidate by providing its address.
		fn do_stake_for_account(
			staker: &T::AccountId,
			candidate: &T::AccountId,
			amount: BalanceOf<T>,
			sort: bool,
		) -> Result<usize, DispatchError> {
			let position = Self::get_candidate(candidate)?;
			Self::do_stake_at_position(staker, amount, position, sort)
		}

		/// Registers a given account as candidate.
		///
		/// The account has to reserve the candidacy bond. If the account was previously a candidate
		/// the retained stake will be reincluded.
		///
		/// Returns the registered candidate.
		pub fn do_register_as_candidate(
			who: &T::AccountId,
		) -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
			let bond = CandidacyBond::<T>::get();

			// In case the staker already had non-claimed stake we calculate it now.
			let mut stakers = 0;
			let already_staked: BalanceOf<T> =
				Stake::<T>::iter_prefix_values(who).fold(Zero::zero(), |acc, s| {
					if !s.is_zero() {
						stakers += 1;
					}
					acc.saturating_add(s)
				});

			// First authored block is current block plus kick threshold to handle session delay
			let candidate = CandidateList::<T>::try_mutate(
				|candidates| -> Result<CandidateInfo<T::AccountId, BalanceOf<T>>, DispatchError> {
					ensure!(
						!candidates.iter().any(|candidate_info| candidate_info.who == *who),
						Error::<T>::AlreadyCandidate
					);
					LastAuthoredBlock::<T>::insert(
						who.clone(),
						Self::current_block_number() + T::KickThreshold::get(),
					);
					let info = CandidateInfo {
						who: who.clone(),
						stake: already_staked,
						deposit: bond,
						stakers,
					};
					T::Currency::hold(&HoldReason::Staking.into(), who, bond)?;
					candidates
						.try_insert(0, info.clone())
						.map_err(|_| Error::<T>::InsertToCandidateListFailed)?;
					PendingExCandidates::<T>::remove(who);
					Ok(info)
				},
			)?;

			Self::deposit_event(Event::CandidateAdded { account_id: who.clone(), deposit: bond });
			Ok(candidate)
		}

		/// Claims all pending unstaking requests for a given user.
		///
		/// Returns the amount of operations performed.
		pub fn do_claim(who: &T::AccountId) -> Result<u32, DispatchError> {
			let mut claimed: BalanceOf<T> = 0u32.into();
			let mut pos = 0;
			UnstakingRequests::<T>::try_mutate(who, |requests| {
				let curr_block = Self::current_block_number();
				for request in requests.iter() {
					if request.block > curr_block {
						break;
					}
					pos += 1;
					T::Currency::release(&HoldReason::Staking.into(), who, request.amount, Exact)?;
					claimed.saturating_accrue(request.amount);
				}
				requests.drain(..pos);
				if !claimed.is_zero() {
					Self::deposit_event(Event::StakeClaimed {
						staker: who.clone(),
						amount: claimed,
					});
				}
				Ok(pos as u32)
			})
		}

		/// Adds stake into a given candidate by providing its position in [`CandidateList`].
		///
		/// Returns the position of the candidate in the list after adding the stake.
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
			ensure!(
				StakeCount::<T>::get(staker) < T::MaxStakedCandidates::get(),
				Error::<T>::TooManyStakedCandidates,
			);
			CandidateList::<T>::try_mutate(|candidates| -> DispatchResult {
				let candidate = &mut candidates[position];
				Stake::<T>::try_mutate(candidate.who.clone(), staker, |stake| -> DispatchResult {
					let final_staker_stake = stake.saturating_add(amount);
					ensure!(
						final_staker_stake >= MinStake::<T>::get(),
						Error::<T>::InsufficientStake
					);
					if stake.is_zero() {
						ensure!(
							candidate.stakers < T::MaxStakers::get(),
							Error::<T>::TooManyStakers
						);
						StakeCount::<T>::mutate(staker, |count| count.saturating_inc());
						candidate.stakers.saturating_inc();
					}
					T::Currency::hold(&HoldReason::Staking.into(), staker, amount)?;
					*stake = final_staker_stake;
					candidate.stake.saturating_accrue(amount);

					Self::deposit_event(Event::StakeAdded {
						staker: staker.clone(),
						candidate: candidate.who.clone(),
						amount,
					});
					Ok(())
				})?;
				Ok(())
			})?;
			let final_position =
				if sort { Self::reassign_candidate_position(position)? } else { position };
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
					.position(|candidate| candidate.stake >= info.stake)
					.unwrap_or_else(|| candidates.len());
				candidates
					.try_insert(new_pos, info)
					.map_err(|_| Error::<T>::InsertToCandidateListFailed)?;
				Ok(new_pos)
			})
		}

		/// Return the total number of accounts that are eligible collators (candidates and
		/// invulnerables).
		pub fn eligible_collators() -> u32 {
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
		/// If the candidate reduces its stake below the [`CandidacyBond`] it will be kicked when
		/// the session ends.
		///
		/// Returns the amount unstaked and the number of unstaking requests the user originally had.
		fn do_unstake(
			staker: &T::AccountId,
			candidate: &T::AccountId,
			has_penalty: bool,
			maybe_position: Option<usize>,
			sort: bool,
		) -> Result<(BalanceOf<T>, u32), DispatchError> {
			let stake = Stake::<T>::take(candidate, staker);
			let mut unstaking_requests = 0;
			ensure!(!stake.is_zero(), Error::<T>::NothingToUnstake);

			if !has_penalty {
				T::Currency::release(&HoldReason::Staking.into(), staker, stake, Exact)?;
			} else {
				let delay = if staker == candidate {
					T::CollatorUnstakingDelay::get()
				} else {
					T::UserUnstakingDelay::get()
				};
				UnstakingRequests::<T>::try_mutate(staker, |requests| -> DispatchResult {
					unstaking_requests = requests.len();
					let block = Self::current_block_number() + delay;
					let pos = requests
						.binary_search_by_key(&block, |r| r.block)
						.unwrap_or_else(|pos| pos);
					requests
						.try_insert(pos, UnstakeRequest { block, amount: stake })
						.map_err(|_| Error::<T>::TooManyUnstakingRequests)?;
					Self::deposit_event(Event::UnstakeRequestCreated {
						staker: staker.clone(),
						candidate: candidate.clone(),
						amount: stake,
						block,
					});
					Ok(())
				})?;
			}
			StakeCount::<T>::mutate_exists(staker, |count| {
				if let Some(c) = count.as_mut() {
					c.saturating_dec();
					match c {
						0 => None,
						_ => Some(*c),
					}
				} else {
					// This should never occur.
					None
				}
			});
			if let Some(position) = maybe_position {
				CandidateList::<T>::mutate(|candidates| {
					candidates[position].stake.saturating_reduce(stake);
					candidates[position].stakers.saturating_dec();
				});
				if sort {
					Self::reassign_candidate_position(position)?;
				}
			}
			Self::deposit_event(Event::StakeRemoved {
				staker: staker.clone(),
				candidate: candidate.clone(),
				amount: stake,
			});

			Ok((stake, unstaking_requests as u32))
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
					let stake = Stake::<T>::get(&candidate.who, &candidate.who);
					if !stake.is_zero() {
						Self::do_unstake(&candidate.who, &candidate.who, has_penalty, None, false)?;
					}

					// Return the bond too.
					if has_penalty {
						UnstakingRequests::<T>::try_mutate(
							&candidate.who,
							|requests| -> DispatchResult {
								requests
									.try_push(UnstakeRequest {
										block: Self::current_block_number()
											+ T::CollatorUnstakingDelay::get(),
										amount: candidate.deposit,
									})
									.map_err(|_| Error::<T>::TooManyUnstakingRequests)?;
								Ok(())
							},
						)?;
					} else {
						T::Currency::release(
							&HoldReason::Staking.into(),
							&candidate.who,
							candidate.deposit,
							Exact,
						)?;
					}

					PendingExCandidates::<T>::set(&candidate.who, true);
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
			let idx = Self::get_candidate(who)?;
			Self::try_remove_candidate_at_position(idx, remove_last_authored, has_penalty)
		}

		/// Distributes the rewards associated for a given collator, obtained during the previous session.
		/// This includes specific rewards for the collator plus rewards for the stakers.
		///
		/// The collator must be a candidate in order to receive the rewards.
		///
		/// Returns the amount of rewarded stakers.
		fn do_reward_collator(
			collator: &T::AccountId,
			blocks: u32,
			session: SessionIndex,
		) -> (bool, u32, u32) {
			let mut total_stakers = 0;
			let mut total_compound = 0;
			if let Ok(pos) = Self::get_candidate(collator) {
				let collator_info = &CandidateList::<T>::get()[pos];
				let total_rewards = Rewards::<T>::get(session);
				let (_, rewardable_blocks) = TotalBlocks::<T>::get(session);
				if rewardable_blocks.is_zero() || collator_info.stake.is_zero() {
					// we cannot divide by zero
					return (true, 0, 0);
				}
				let collator_percentage = CollatorRewardPercentage::<T>::get();

				let rewards_all: BalanceOf<T> =
					total_rewards.saturating_mul(blocks.into()) / rewardable_blocks.into();
				let collator_only_reward = collator_percentage.mul_floor(rewards_all);

				// Reward collator. Note these rewards are not autocompounded.
				if let Err(error) = Self::do_reward_single(collator, collator_only_reward) {
					log::warn!(target: LOG_TARGET, "Failure rewarding collator {:?}: {:?}", collator, error);
				}

				// Reward stakers
				let stakers_only_rewards = total_rewards.saturating_sub(collator_only_reward);
				Stake::<T>::iter_prefix(collator).for_each(|(staker, stake)| {
					total_stakers += 1;
					let staker_reward: BalanceOf<T> =
						Perbill::from_rational(stake, collator_info.stake) * stakers_only_rewards;
					if let Err(error) = Self::do_reward_single(&staker, staker_reward) {
						log::warn!(target: LOG_TARGET, "Failure rewarding staker {:?}: {:?}", staker, error);
					} else {
						// AutoCompound
						total_compound += 1;
						let compound_percentage = AutoCompound::<T>::get(staker.clone());
						let compound_amount = compound_percentage.mul_floor(staker_reward);
						if !compound_amount.is_zero() {
							if let Err(error) =
								Self::do_stake_at_position(&staker, compound_amount, pos, false)
							{
								log::warn!(
									target: LOG_TARGET,
									"Failure autocompounding for staker {:?} to candidate {:?}: {:?}",
									staker,
									collator,
									error
								);
							}
						}
					}
				});
				if !total_compound.is_zero() {
					// No need to sort again if no new investments were made.
					let _ = Self::reassign_candidate_position(pos);
				}
			} else {
				log::warn!("Collator {:?} is no longer a candidate", collator);
			}
			(true, total_stakers, total_compound)
		}

		fn do_reward_single(who: &T::AccountId, reward: BalanceOf<T>) -> DispatchResult {
			T::Currency::transfer(&Self::account_id(), who, reward, Preserve)?;
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
		pub fn kick_stale_candidates() -> u32 {
			let now = Self::current_block_number();
			let kick_threshold = T::KickThreshold::get();
			let min_collators = T::MinEligibleCollators::get();
			let candidacy_bond = CandidacyBond::<T>::get();
			let candidates = CandidateList::<T>::get();
			candidates
                .into_iter()
                .filter_map(|candidate| {
                    let last_block = LastAuthoredBlock::<T>::get(candidate.who.clone());
                    let since_last = now.saturating_sub(last_block);

                    let is_invulnerable = Self::is_invulnerable(&candidate.who);
                    let is_lazy = since_last >= kick_threshold;

                    if is_invulnerable {
                        // If they are invulnerable there is no reason for them to be in `CandidateList` also.
                        // We don't even care about the min collators here, because an Account
                        // should not be a collator twice.
                        let _ = Self::try_remove_candidate_from_account(&candidate.who, false, false);
                        None
                    } else if Self::eligible_collators() <= min_collators || (!is_lazy && candidate.deposit.saturating_add(candidate.stake) >= candidacy_bond) {
                        // Either this is a good collator (not lazy) or we are at the minimum
                        // that the system needs. They get to stay, as long as they have sufficient deposit plus stake.
                        Some(candidate)
                    } else {
                        // This collator has not produced a block recently enough. Bye bye.
                        let _ = Self::try_remove_candidate_from_account(&candidate.who, true, true);
                        None
                    }
                })
                .count()
                .try_into()
                .expect("filter_map operation can't result in a bounded vec larger than its original; qed")
		}

		/// Rewards a pending collator from the previous round, if any.
		///
		/// Returns a tuple with the number of rewards given and the number of auto compounds.
		pub(crate) fn reward_one_collator(session: SessionIndex) -> (u32, u32) {
			let mut iter = ProducedBlocks::<T>::iter_prefix(session);
			if let Some((collator, blocks)) = iter.next() {
				let (succeed, rewards, compounds) =
					Self::do_reward_collator(&collator, blocks, session);
				if succeed {
					ProducedBlocks::<T>::remove(session, collator.clone());
				}
				(rewards, compounds)
			} else {
				(0, 0)
			}
		}

		/// Refunds any stake deposited in a given ex-candidate to the corresponding stakers.
		///
		/// Returns the amount of refunded stakers.
		pub(crate) fn refund_stakers(account: &T::AccountId) -> u32 {
			let count = Stake::<T>::iter_prefix(account)
				.filter_map(|(staker, amount)| {
					if !amount.is_zero() {
						if let Err(e) = Self::do_unstake(&staker, account, false, None, false) {
							// This should never occur.
							log::warn!(
								"Could not unstake staker {:?} from candidate {:?}: {:?}",
								staker,
								account,
								e
							);
						}
						Some(())
					} else {
						None
					}
				})
				.count() as u32;
			let _ = Stake::<T>::clear_prefix(&account, u32::MAX, None);
			count
		}

		/// Ensure the correctness of the state of this pallet.
		///
		/// This should be valid before or after each state transition of this pallet.
		///
		/// # Invariants
		///
		/// ## [`DesiredCandidates`]
		///
		/// * The current desired candidate count should not exceed the candidate list capacity.
		/// * The number of selected candidates together with the invulnerables must be greater than
		///   or equal to the minimum number of eligible collators.
		///
		/// ## [`MaxCandidates`]
		///
		/// * The amount of stakers per account is limited and its maximum value must not be surpassed.
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

			frame_support::ensure!(
				StakeCount::<T>::iter_values().all(|count| count < T::MaxStakedCandidates::get()),
				"Stake count must not exceed MaxStakedCandidates"
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

			// Invulnerables do not get rewards
			if Self::is_invulnerable(&author) {
				TotalBlocks::<T>::mutate(current_session, |(total, _)| {
					total.saturating_inc();
				});
			} else {
				ProducedBlocks::<T>::mutate(current_session, author, |b| b.saturating_inc());
				TotalBlocks::<T>::mutate(current_session, |(total, rewardable)| {
					total.saturating_inc();
					rewardable.saturating_inc();
				});
			}

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
				target: LOG_TARGET,
				"assembling new collators for new session {} at #{:?}",
				index,
				frame_system::Pallet::<T>::block_number(),
			);

			// The `expect` below is safe because the list is a `BoundedVec` with a max size of
			// `T::MaxCandidates`, which is a `u32`. When `decode_len` returns `Some(len)`, `len`
			// must be valid and at most `u32::MAX`, which must always be able to convert to `u32`.
			let candidates_len_before: u32 = CandidateList::<T>::decode_len()
				.unwrap_or_default()
				.try_into()
				.expect("length is at most `T::MaxCandidates`, so it must fit in `u32`; qed");
			let active_candidates_count = Self::kick_stale_candidates();
			let removed = candidates_len_before.saturating_sub(active_candidates_count);
			let result = Self::assemble_collators();

			frame_system::Pallet::<T>::register_extra_weight_unchecked(
				T::WeightInfo::new_session(candidates_len_before, removed),
				DispatchClass::Mandatory,
			);
			Some(result)
		}

		fn start_session(index: SessionIndex) {
			// Initialize counters for this session
			TotalBlocks::<T>::insert(index, (0, 0));
			CurrentSession::<T>::put(index);

			// cleanup last session's stuff
			if index > 1 {
				let last_session = index - 2;
				TotalBlocks::<T>::remove(last_session);
				Rewards::<T>::remove(last_session);
				let _ = ProducedBlocks::<T>::clear_prefix(last_session, u32::MAX, None);
			}
		}

		fn end_session(index: SessionIndex) {
			// Transfer the extra reward, if any, to the pot.
			let pot_account = Self::account_id();
			let per_block_extra_reward = ExtraReward::<T>::get();
			if !per_block_extra_reward.is_zero() {
				let (produced_blocks, _) = TotalBlocks::<T>::get(index);
				let extra_reward = per_block_extra_reward.saturating_mul(produced_blocks.into());
				if let Err(error) = T::Currency::transfer(
					&Self::extra_reward_account_id(),
					&pot_account,
					extra_reward,
					Expendable, // we do not care if the extra reward pot gets destroyed.
				) {
					log::warn!(target: LOG_TARGET, "Failure transferring extra rewards to the pallet-collator-staking pot account: {:?}", error);
				}
			}

			// Rewards are the total amount in the pot minus the existential deposit.
			let total_rewards =
				T::Currency::balance(&pot_account).saturating_sub(T::Currency::minimum_balance());
			Rewards::<T>::insert(index, total_rewards);
			Self::deposit_event(Event::<T>::SessionEnded { index, rewards: total_rewards });
		}
	}
}

/// [`TypedGet`] implementation to get the AccountId of the StakingPot.
pub struct StakingPotAccountId<R>(PhantomData<R>);
impl<R> TypedGet for StakingPotAccountId<R>
where
	R: Config,
{
	type Type = <R as frame_system::Config>::AccountId;
	fn get() -> Self::Type {
		Pallet::<R>::account_id()
	}
}
