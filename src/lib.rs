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

use codec::Codec;
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
	use frame_support::{
		dispatch::{DispatchClass, DispatchResultWithPostInfo},
		pallet_prelude::*,
		traits::{
			fungible::{Inspect, InspectFreeze, Mutate, MutateFreeze},
			tokens::Fortitude::Polite,
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
	use sp_std::vec::Vec;

	pub use crate::weights::WeightInfo;

	use super::LOG_TARGET;

	/// The in-code storage version.
	const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

	/// A convertor from collators id. Since this pallet does not have stash/controller, this is
	/// just identity.
	pub struct IdentityCollator;

	impl<T> Convert<T, Option<T>> for IdentityCollator {
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
			+ InspectFreeze<Self::AccountId>
			+ Mutate<Self::AccountId>
			+ MutateFreeze<Self::AccountId, Id = Self::RuntimeFreezeReason>;

		/// Overarching freeze reason.
		type RuntimeFreezeReason: From<FreezeReason>;

		/// Origin that can dictate updating parameters of this pallet.
		type UpdateOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		/// Account Identifier from which the internal pot is generated.
		///
		/// To initiate rewards, an ED needs to be transferred to the pot address.
		#[pallet::constant]
		type PotId: Get<PalletId>;

		/// Account Identifier from which the extra reward pot is generated.
		///
		/// To initiate extra rewards the [`set_extra_reward`] extrinsic must be called;
		/// and this pot should be funded using [`top_up_extra_rewards`] extrinsic.
		#[pallet::constant]
		type ExtraRewardPotId: Get<PalletId>;

		/// Determines what to do with funds in the extra rewards pot when stopping these rewards.
		#[pallet::constant]
		type ExtraRewardReceiver: Get<Option<Self::AccountId>>;

		/// Maximum number of candidates that we should have.
		///
		/// This does not take into account the invulnerables.
		/// This must be more than or equal to `DesiredCandidates`.
		#[pallet::constant]
		type MaxCandidates: Get<u32>;

		/// Minimum number eligible collators including Invulnerables.
		/// Should always be greater than zero. This ensures that there will always be
		/// one collator who can produce blocks.
		#[pallet::constant]
		type MinEligibleCollators: Get<u32>;

		/// Maximum number of invulnerables.
		#[pallet::constant]
		type MaxInvulnerables: Get<u32>;

		/// Candidates will be  removed from active collator set, if block is not produced within this threshold.
		#[pallet::constant]
		type KickThreshold: Get<BlockNumberFor<Self>>;

		/// A stable ID for a collator.
		type CollatorId: Member + Parameter;

		/// A conversion from account ID to collator ID.
		///
		/// Its cost must be at most one storage read.
		type CollatorIdOf: Convert<Self::AccountId, Option<Self::CollatorId>>;

		/// Validate a collator is registered.
		type CollatorRegistration: ValidatorRegistration<Self::CollatorId>;

		/// Maximum candidates a staker can stake on.
		#[pallet::constant]
		type MaxStakedCandidates: Get<u32>;

		/// Maximum stakers per candidate.
		#[pallet::constant]
		type MaxStakers: Get<u32>;

		/// Number of blocks to wait before returning the bond by a candidate.
		#[pallet::constant]
		type BondUnlockDelay: Get<BlockNumberFor<Self>>;

		/// Number of blocks to wait before returning the locked funds by a user.
		#[pallet::constant]
		type StakeUnlockDelay: Get<BlockNumberFor<Self>>;

		/// The weight information of this pallet.
		type WeightInfo: WeightInfo;
	}

	/// A reason for the pallet to freeze funds.
	#[pallet::composite_enum]
	pub enum FreezeReason {
		Staking,
		CandidacyBond,
		Releasing,
	}

	/// Basic information about a collator candidate.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct CandidateInfo<Balance> {
		/// Total stake.
		pub stake: Balance,
		/// Amount of stakers.
		pub stakers: u32,
	}

	/// Information about the release requests.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct ReleaseRequest<BlockNumber, Balance> {
		/// Block when stake can be unlocked.
		pub block: BlockNumber,
		/// Stake to be unlocked.
		pub amount: Balance,
	}

	/// Information about a candidate's stake.
	#[derive(
		Default,
		PartialEq,
		Eq,
		Clone,
		Encode,
		Decode,
		RuntimeDebug,
		scale_info::TypeInfo,
		MaxEncodedLen,
	)]
	pub struct CandidateStakeInfo<Balance> {
		/// Session when the user first staked on a given candidate.
		pub session: SessionIndex,
		/// The amount staked.
		pub stake: Balance,
	}

	/// Information about a users' stake.
	#[derive(
		Default,
		PartialEq,
		Eq,
		Clone,
		Encode,
		Decode,
		RuntimeDebug,
		scale_info::TypeInfo,
		MaxEncodedLen,
	)]
	pub struct UserStakeInfo<Balance> {
		/// Total candidates staked.
		pub count: u32,
		/// The total amount staked in all candidates.
		pub stake: Balance,
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
	/// This list is sorted in ascending order by total stake and when the stake amounts are equal, the least
	/// recently updated is considered greater.
	#[pallet::storage]
	pub type Candidates<T: Config> = CountedStorageMap<
		_,
		Blake2_128Concat,
		T::AccountId,
		CandidateInfo<BalanceOf<T>>,
		OptionQuery,
	>;

	/// Last block authored by a collator.
	#[pallet::storage]
	pub type LastAuthoredBlock<T: Config> =
		StorageMap<_, Twox64Concat, T::AccountId, BlockNumberFor<T>, ValueQuery>;

	/// Desired number of candidates.
	///
	/// This should always be less than [`Config::MaxCandidates`] for weights to be correct.
	///
	/// IMP: This must be less than the session length,
	/// because rewards are distributed for one collator per block.
	#[pallet::storage]
	pub type DesiredCandidates<T> = StorageValue<_, u32, ValueQuery>;

	/// Minimum amount to become a collator.
	#[pallet::storage]
	pub type MinCandidacyBond<T> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Minimum amount a user can stake.
	#[pallet::storage]
	pub type MinStake<T> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Amount staked by users per candidate.
	///
	/// First key is the candidate, and second one is the staker.
	#[pallet::storage]
	pub type CandidateStake<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		T::AccountId,
		Blake2_128Concat,
		T::AccountId,
		CandidateStakeInfo<BalanceOf<T>>,
		ValueQuery,
	>;

	/// Number of candidates staked on by a user.
	///
	/// Cannot be higher than `MaxStakedCandidates`.
	#[pallet::storage]
	pub type UserStake<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, UserStakeInfo<BalanceOf<T>>, ValueQuery>;

	/// Release requests for an account.
	///
	/// They can be claimed by calling the [`claim`] extrinsic, after the relevant delay.
	#[pallet::storage]
	pub type ReleaseQueues<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		T::AccountId,
		BoundedVec<ReleaseRequest<BlockNumberFor<T>, BalanceOf<T>>, T::MaxStakedCandidates>,
		ValueQuery,
	>;

	/// Percentage of rewards that would go for collators.
	#[pallet::storage]
	pub type CollatorRewardPercentage<T: Config> = StorageValue<_, Percent, ValueQuery>;

	/// Per-block extra reward.
	#[pallet::storage]
	pub type ExtraReward<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Blocks produced in the current session. First value the total,
	/// and second is blocks produced by candidates only (not invulnerables).
	#[pallet::storage]
	pub type TotalBlocks<T: Config> =
		StorageMap<_, Blake2_128Concat, SessionIndex, (u32, u32), ValueQuery>;

	/// Rewards generated for a given session.
	#[pallet::storage]
	pub type Rewards<T: Config> =
		StorageMap<_, Blake2_128Concat, SessionIndex, BalanceOf<T>, ValueQuery>;

	/// Mapping of blocks and their authors.
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
			MinCandidacyBond::<T>::put(self.candidacy_bond);
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
		/// The minimum candidacy bond was set.
		NewMinCandidacyBond { bond_amount: BalanceOf<T> },
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
		ReleaseRequestCreated {
			staker: T::AccountId,
			candidate: T::AccountId,
			amount: BalanceOf<T>,
			block: BlockNumberFor<T>,
		},
		/// A staker removed stake from a candidate
		StakeRemoved { staker: T::AccountId, candidate: T::AccountId, amount: BalanceOf<T> },
		/// A staking reward was delivered.
		StakingRewardReceived { staker: T::AccountId, amount: BalanceOf<T>, session: SessionIndex },
		/// AutoCompound percentage was set.
		AutoCompoundPercentageSet { staker: T::AccountId, percentage: Percent },
		/// Collator reward percentage was set.
		CollatorRewardPercentageSet { percentage: Percent },
		/// The extra reward was set.
		ExtraRewardSet { amount: BalanceOf<T> },
		/// The extra reward was removed.
		ExtraRewardRemoved { amount_left: BalanceOf<T>, receiver: Option<T::AccountId> },
		/// The minimum amount to stake was changed.
		NewMinStake { min_stake: BalanceOf<T> },
		/// A session just ended.
		SessionEnded { index: SessionIndex, rewards: BalanceOf<T> },
		/// The extra reward pot account was funded.
		ExtraRewardPotFunded { pot: T::AccountId, amount: BalanceOf<T> },
		/// The staking locked amount got extended.
		LockExtended { total: BalanceOf<T> },
		/// The staking locked amount got decreased.
		LockDecreased { total: BalanceOf<T> },
		/// A candidate's candidacy bond got updated.
		CandidacyBondUpdated { candidate: T::AccountId, new_bond: BalanceOf<T> },
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
		/// Amount not sufficient to be staked.
		InsufficientStake,
		/// DesiredCandidates is out of bounds.
		TooManyDesiredCandidates,
		/// Too many unstaking requests. Claim some of them first.
		TooManyReleaseRequests,
		/// Cannot take some candidate's slot while the candidate list is not full.
		CanRegister,
		/// Invalid value for MinStake. It must be lower than or equal to `MinStake`.
		InvalidMinStake,
		/// Invalid value for CandidacyBond. It must be higher than or equal to `MinCandidacyBond`.
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
		/// The user does not have enough balance to be locked for staking.
		InsufficientFreeBalance,
		/// The user does not have enough locked balance to stake.
		InsufficientLockedBalance,
		/// Cannot unlock such amount.
		CannotUnlock,
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
				T::MaxCandidates::get() >= T::MaxStakedCandidates::get(),
				"MaxCandidates must be greater than or equal to MaxStakedCandidates"
			);
		}

		/// Rewards are delivered at the beginning of each block. The underlined assumption is that
		/// the number of collators to be rewarded is lower than the number of blocks in
		/// a given session.
		///
		/// Only one collator and its stakers are rewarded per block, until all
		/// collators (and their stakers) are rewarded for the previous session.
		fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
			let mut weight = T::DbWeight::get().reads_writes(1, 0);
			let current_session = CurrentSession::<T>::get();
			if current_session > 0 {
				let (rewarded_stakers, compounded_stakers) =
					Self::reward_one_collator(current_session - 1);
				if !rewarded_stakers.is_zero() {
					weight = weight.saturating_add(T::WeightInfo::reward_one_collator(
						Candidates::<T>::count(),
						rewarded_stakers,
						compounded_stakers * 100 / rewarded_stakers,
					));
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
					Candidates::<T>::count() >= T::MinEligibleCollators::get(),
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
				// If at least one of the invulnerables is already a collator abort the operation.
				ensure!(Self::get_candidate(account_id).is_err(), Error::<T>::AlreadyCandidate);
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

		/// Set the candidacy bond amount, which represents the required amount to reserve for an
		/// account to become a candidate. The candidacy bond does not count as stake.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::set_candidacy_bond())]
		pub fn set_min_candidacy_bond(origin: OriginFor<T>, bond: BalanceOf<T>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			MinCandidacyBond::<T>::put(bond);
			Self::deposit_event(Event::NewMinCandidacyBond { bond_amount: bond });
			Ok(())
		}

		/// Register this account as a collator candidate. The account must (a) already have
		/// registered session keys and (b) be able to reserve the `CandidacyBond`.
		/// The `CandidacyBond` amount is automatically reserved from the balance of the caller.
		///
		/// This call is not available to `Invulnerable` collators.
		#[pallet::call_index(3)]
		#[pallet::weight(T::WeightInfo::register_as_candidate(T::MaxCandidates::get()))]
		pub fn register_as_candidate(
			origin: OriginFor<T>,
			bond: BalanceOf<T>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;

			// ensure we are below limit.
			let length = Candidates::<T>::count();
			ensure!(length < T::MaxCandidates::get(), Error::<T>::TooManyCandidates);
			ensure!(!Self::is_invulnerable(&who), Error::<T>::AlreadyInvulnerable);

			let validator_key =
				T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
			ensure!(
				T::CollatorRegistration::is_registered(&validator_key),
				Error::<T>::CollatorNotRegistered
			);

			Self::do_register_as_candidate(&who, bond)?;
			// Safe to do unchecked add here because we ensure above that `length <
			// T::MaxCandidates::get()`, and since `T::MaxCandidates` is `u32` it can be at most
			// `u32::MAX`, therefore `length + 1` cannot overflow.
			Ok(Some(T::WeightInfo::register_as_candidate(length + 1)).into())
		}

		/// Deregister `origin` as a collator candidate. No rewards will be delivered to this
		/// candidate and its stakers after this moment.
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
			let length = Candidates::<T>::count();
			// Do remove their last authored block.
			Self::try_remove_candidate(&who, true, true)?;

			Ok(Some(T::WeightInfo::leave_intent(length.saturating_sub(1))).into())
		}

		/// Add a new account `who` to the list of `Invulnerables` collators. `who` must have
		/// registered session keys. If `who` is a candidate, it will be removed.
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

			// If the account is already a candidate this operation cannot be performed.
			ensure!(Self::get_candidate(&who).is_err(), Error::<T>::AlreadyCandidate);

			Invulnerables::<T>::try_mutate(|invulnerables| -> DispatchResult {
				match invulnerables.binary_search(&who) {
					Ok(_) => return Err(Error::<T>::AlreadyInvulnerable)?,
					Err(pos) => invulnerables
						.try_insert(pos, who.clone())
						.map_err(|_| Error::<T>::TooManyInvulnerables)?,
				}
				Ok(())
			})?;

			Self::deposit_event(Event::InvulnerableAdded { account_id: who });

			let weight_used = T::WeightInfo::add_invulnerable(
				Invulnerables::<T>::decode_len()
					.unwrap_or_default()
					.try_into()
					.unwrap_or(T::MaxInvulnerables::get().saturating_sub(1)),
				Candidates::<T>::count(),
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

		/// Allows a user to stake on a collator candidate.
		///
		/// The call will fail if:
		///     - `origin` does not have the at least `MinStake` deposited in the candidate.
		///     - `candidate` is not in the [`Candidates`].
		#[pallet::call_index(7)]
		#[pallet::weight(T::WeightInfo::stake(T::MaxCandidates::get()))]
		pub fn stake(
			origin: OriginFor<T>,
			candidate: T::AccountId,
			stake: BalanceOf<T>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			Self::do_stake(&who, &candidate, stake)?;
			Ok(Some(T::WeightInfo::stake(Candidates::<T>::count())).into())
		}

		/// Removes stake from a collator candidate.
		///
		/// If the candidate is an active collator, the caller will get the funds after a delay. Otherwise,
		/// funds will be returned immediately.
		///
		/// The candidate will have its position in the [`Candidates`] updated.
		#[pallet::call_index(8)]
		#[pallet::weight(T::WeightInfo::unstake_from(T::MaxCandidates::get(),))]
		pub fn unstake_from(
			origin: OriginFor<T>,
			candidate: T::AccountId,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let _ = Self::do_unstake(&who, &candidate)?;
			Ok(Some(T::WeightInfo::unstake_from(Candidates::<T>::count())).into())
		}

		/// Removes all stake of a user from all candidates.
		#[pallet::call_index(9)]
		#[pallet::weight(T::WeightInfo::unstake_all(
			T::MaxCandidates::get(),
			T::MaxStakedCandidates::get()
		))]
		pub fn unstake_all(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let mut operations = 0;
			for account in Candidates::<T>::iter_keys() {
				let stake = Self::do_unstake(&who, &account)?;
				if !stake.is_zero() {
					operations += 1;
				}
			}
			Ok(Some(T::WeightInfo::unstake_all(Candidates::<T>::count(), operations)).into())
		}

		/// Claims all pending [`ReleaseRequest`] for a given account.
		#[pallet::call_index(10)]
		#[pallet::weight(T::WeightInfo::claim(T::MaxStakedCandidates::get()))]
		pub fn claim(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let operations = Self::do_claim(&who)?;
			Ok(Some(T::WeightInfo::claim(operations)).into())
		}

		/// Sets the percentage of rewards that should be auto-compounded.
		#[pallet::call_index(11)]
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

		/// Sets the percentage of rewards that collators will take for producing blocks.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(12)]
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
		#[pallet::call_index(13)]
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

		/// Sets minimum amount that can be staked on a candidate.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(14)]
		#[pallet::weight(T::WeightInfo::set_minimum_stake())]
		pub fn set_minimum_stake(
			origin: OriginFor<T>,
			new_min_stake: BalanceOf<T>,
		) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			ensure!(new_min_stake <= MinCandidacyBond::<T>::get(), Error::<T>::InvalidMinStake);

			MinStake::<T>::put(new_min_stake);
			Self::deposit_event(Event::NewMinStake { min_stake: new_min_stake });
			Ok(())
		}

		/// Stops the extra rewards.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(15)]
		#[pallet::weight(T::WeightInfo::stop_extra_reward())]
		pub fn stop_extra_reward(origin: OriginFor<T>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;

			let extra_reward = ExtraReward::<T>::get();
			ensure!(!extra_reward.is_zero(), Error::<T>::ExtraRewardAlreadyDisabled);

			ExtraReward::<T>::kill();

			let pot = Self::extra_reward_account_id();
			let balance = T::Currency::reducible_balance(&pot, Expendable, Polite);
			let receiver = T::ExtraRewardReceiver::get();
			if !balance.is_zero() {
				if let Some(ref receiver) = receiver {
					if let Err(error) = T::Currency::transfer(&pot, receiver, balance, Expendable) {
						// We should not cancel the operation if we cannot transfer funds from the pot,
						// as it is more important to stop the rewards.
						log::warn!("Failure transferring extra reward pot remaining balance to the destination account {:?}: {:?}", receiver, error);
					}
				}
			}
			Self::deposit_event(Event::ExtraRewardRemoved { amount_left: balance, receiver });
			Ok(())
		}

		/// Funds the extra reward pot account.
		#[pallet::call_index(16)]
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

		/// Locks free funds to be used for staking.
		#[pallet::call_index(17)]
		#[pallet::weight({0})]
		pub fn lock(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;

			let available_balance = Self::get_free_balance(&who);
			ensure!(available_balance >= amount, Error::<T>::InsufficientFreeBalance);

			let total = Self::get_staked_balance(&who).saturating_add(amount);
			T::Currency::set_freeze(&FreezeReason::Staking.into(), &who, total)?;

			Self::deposit_event(Event::<T>::LockExtended { total });

			Ok(())
		}

		/// Unlocks funds used for staking and queues them to be claimed.
		#[pallet::call_index(18)]
		#[pallet::weight({0})]
		pub fn unlock(origin: OriginFor<T>, maybe_amount: Option<BalanceOf<T>>) -> DispatchResult {
			let who = ensure_signed(origin)?;

			let UserStakeInfo { stake: total_staked, .. } = UserStake::<T>::get(&who);
			let staked_balance = Self::get_staked_balance(&who);
			let available = staked_balance.saturating_sub(total_staked);
			let amount = if let Some(desired_amount) = maybe_amount {
				ensure!(available >= desired_amount, Error::<T>::CannotUnlock);
				desired_amount
			} else {
				available
			};
			T::Currency::set_freeze(
				&FreezeReason::Staking.into(),
				&who,
				staked_balance.saturating_sub(amount),
			)?;
			Self::add_to_release_queue(&who, amount, T::StakeUnlockDelay::get())?;

			Ok(())
		}

		/// Updates the candidacy bond for this candidate.
		#[pallet::call_index(19)]
		#[pallet::weight({0})]
		pub fn update_candidacy_bond(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;
			ensure!(amount >= MinCandidacyBond::<T>::get(), Error::<T>::InvalidCandidacyBond);
			ensure!(Self::get_candidate(&who).is_ok(), Error::<T>::NotCandidate);

			let available_balance =
				Self::get_releasing_balance(&who).saturating_add(Self::get_staked_balance(&who));
			ensure!(available_balance >= amount, Error::<T>::InsufficientFreeBalance);

			T::Currency::extend_freeze(&FreezeReason::CandidacyBond.into(), &who, amount)?;

			Self::deposit_event(Event::<T>::CandidacyBondUpdated {
				candidate: who,
				new_bond: amount,
			});

			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {
		/// Get a unique, inaccessible account ID from the `PotId`.
		pub fn account_id() -> T::AccountId {
			T::PotId::get().into_account_truncating()
		}

		/// Get a unique, inaccessible account ID from the `ExtraRewardPotId`.
		pub fn extra_reward_account_id() -> T::AccountId {
			T::ExtraRewardPotId::get().into_account_truncating()
		}

		/// Checks whether a given account is a candidate and returns its position if successful.
		pub fn get_candidate(
			account: &T::AccountId,
		) -> Result<CandidateInfo<BalanceOf<T>>, DispatchError> {
			Candidates::<T>::get(account).ok_or(Error::<T>::NotCandidate.into())
		}

		/// Checks whether a given account is an invulnerable.
		pub fn is_invulnerable(account: &T::AccountId) -> bool {
			Invulnerables::<T>::get().binary_search(account).is_ok()
		}

		/// Registers a given account as candidate.
		///
		/// The account has to reserve the candidacy bond. If the account was previously a candidate
		/// the retained stake will be re-included.
		///
		/// Returns the registered candidate.
		pub fn do_register_as_candidate(
			who: &T::AccountId,
			bond: BalanceOf<T>,
		) -> Result<CandidateInfo<BalanceOf<T>>, DispatchError> {
			let min_bond = MinCandidacyBond::<T>::get();
			ensure!(bond >= min_bond, Error::<T>::InvalidCandidacyBond);

			let available_balance = Self::get_free_balance(who);
			ensure!(available_balance >= bond, Error::<T>::InsufficientFreeBalance);

			// First authored block is current block plus kick threshold to handle session delay
			let candidate = Candidates::<T>::try_mutate_exists(
				who,
				|maybe_candidate_info| -> Result<CandidateInfo<BalanceOf<T>>, DispatchError> {
					ensure!(maybe_candidate_info.is_none(), Error::<T>::AlreadyCandidate);
					LastAuthoredBlock::<T>::insert(
						who.clone(),
						Self::current_block_number() + T::KickThreshold::get(),
					);
					let info = CandidateInfo { stake: 0u32.into(), stakers: 0 };
					*maybe_candidate_info = Some(info.clone());
					T::Currency::set_freeze(&FreezeReason::CandidacyBond.into(), who, bond)?;
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
			ReleaseQueues::<T>::mutate_exists(who, |maybe_requests| {
				if let Some(requests) = maybe_requests {
					let curr_block = Self::current_block_number();
					for request in requests.iter() {
						if request.block > curr_block {
							break;
						}
						pos += 1;
						claimed.saturating_accrue(request.amount);
					}
					requests.drain(..pos);
					return if requests.is_empty() { None } else { Some(()) };
				}
				None
			});
			if !claimed.is_zero() {
				let releasing_balance = Self::get_releasing_balance(who);
				T::Currency::set_freeze(
					&FreezeReason::Releasing.into(),
					who,
					releasing_balance.saturating_sub(claimed),
				)?;
				Self::deposit_event(Event::StakeClaimed { staker: who.clone(), amount: claimed });
			}
			Ok(pos as u32)
		}

		/// Adds stake into a given candidate by providing its address.
		fn do_stake(
			staker: &T::AccountId,
			candidate: &T::AccountId,
			amount: BalanceOf<T>,
		) -> Result<(), DispatchError> {
			let UserStakeInfo { count, stake: currently_staked } = UserStake::<T>::get(staker);
			let frozen_balance = Self::get_staked_balance(staker);
			ensure!(count < T::MaxStakedCandidates::get(), Error::<T>::TooManyStakedCandidates);
			ensure!(
				frozen_balance.saturating_sub(currently_staked) >= amount,
				Error::<T>::InsufficientLockedBalance
			);

			Candidates::<T>::try_mutate(candidate, |maybe_candidate_info| -> DispatchResult {
				let mut candidate_info =
					maybe_candidate_info.clone().ok_or(Error::<T>::NotCandidate)?;
				CandidateStake::<T>::try_mutate(candidate, staker, |info| -> DispatchResult {
					let final_staker_stake = info.stake.saturating_add(amount);
					ensure!(
						final_staker_stake >= MinStake::<T>::get(),
						Error::<T>::InsufficientStake
					);
					if info.stake.is_zero() {
						ensure!(
							candidate_info.stakers < T::MaxStakers::get(),
							Error::<T>::TooManyStakers
						);
						UserStake::<T>::mutate(staker, |info| {
							info.count.saturating_inc();
							info.stake.saturating_accrue(amount);
						});
						candidate_info.stakers.saturating_inc();
						info.session = CurrentSession::<T>::get();
					}
					info.stake = final_staker_stake;
					candidate_info.stake.saturating_accrue(amount);

					Self::deposit_event(Event::StakeAdded {
						staker: staker.clone(),
						candidate: candidate.clone(),
						amount,
					});
					Ok(())
				})?;
				*maybe_candidate_info = Some(candidate_info);
				Ok(())
			})
		}

		/// Return the total number of accounts that are eligible collators (candidates and
		/// invulnerables).
		pub fn eligible_collators() -> u32 {
			Candidates::<T>::count()
				.saturating_add(Invulnerables::<T>::decode_len().unwrap_or_default() as u32)
		}

		/// Unstakes all funds deposited in a given `candidate`.
		///
		/// Returns the amount unstaked.
		fn do_unstake(
			staker: &T::AccountId,
			candidate: &T::AccountId,
		) -> Result<BalanceOf<T>, DispatchError> {
			ensure!(Candidates::<T>::get(candidate).is_some(), Error::<T>::NotCandidate);
			let stake = Self::remove_stake(candidate, staker);

			if !stake.is_zero() {
				Candidates::<T>::mutate(candidate, |maybe_info| {
					if let Some(info) = maybe_info {
						info.stake.saturating_reduce(stake);
						info.stakers.saturating_dec();
					}
				});
			}

			Ok(stake)
		}

		/// Removes stake from a given candidate.
		///
		/// Returns the amount of stake removed.
		fn remove_stake(candidate: &T::AccountId, staker: &T::AccountId) -> BalanceOf<T> {
			let mut stake = Zero::zero();
			CandidateStake::<T>::mutate_exists(candidate, staker, |maybe_candidate_stake_info| {
				if let Some(candidate_stake_info) = maybe_candidate_stake_info {
					stake = candidate_stake_info.stake;
					UserStake::<T>::mutate_exists(staker, |maybe_user_stake_info| {
						if let Some(user_stake_info) = maybe_user_stake_info {
							match user_stake_info.count {
								0..=1 => *maybe_user_stake_info = None,
								_ => {
									*maybe_user_stake_info = Some(UserStakeInfo {
										count: user_stake_info.count.saturating_sub(1),
										stake: user_stake_info
											.stake
											.saturating_sub(candidate_stake_info.stake),
									});
								},
							}
						} else {
							// This should never occur.
							*maybe_user_stake_info = None;
						}
					});
				}
				*maybe_candidate_stake_info = None;
			});
			Self::deposit_event(Event::StakeRemoved {
				staker: staker.clone(),
				candidate: candidate.clone(),
				amount: stake,
			});
			stake
		}

		/// Attempts to remove a candidate, identified by its account, if it exists and refunds the stake.
		///
		/// Returns the candidate info.
		fn try_remove_candidate(
			who: &T::AccountId,
			remove_last_authored: bool,
			has_penalty: bool,
		) -> Result<CandidateInfo<BalanceOf<T>>, DispatchError> {
			Candidates::<T>::try_mutate_exists(
				who,
				|maybe_candidate| -> Result<CandidateInfo<BalanceOf<T>>, DispatchError> {
					let candidate = maybe_candidate.clone().ok_or(Error::<T>::NotCandidate)?;
					// This is an expensive operation.
					let stakers = CandidateStake::<T>::iter_prefix(who)
						.map(|(staker, _)| staker)
						.collect::<Vec<_>>();
					for staker in stakers {
						Self::remove_stake(who, &staker);
					}
					if remove_last_authored {
						LastAuthoredBlock::<T>::remove(who.clone())
					};

					// We firstly optimistically release the candidacy bond.
					let amount = Self::get_bond(who);
					T::Currency::set_freeze(
						&FreezeReason::CandidacyBond.into(),
						who,
						Zero::zero(),
					)?;
					// If it has a penalty then we lock the funds we just unlocked and add them
					// to the release queue.
					if has_penalty {
						Self::add_to_release_queue(who, amount, T::BondUnlockDelay::get())?;
					}

					Self::deposit_event(Event::CandidateRemoved { account_id: who.clone() });
					*maybe_candidate = None;
					Ok(candidate)
				},
			)
		}

		/// Adds locked funds not invested to the release queue for a given user.
		fn add_to_release_queue(
			account: &T::AccountId,
			amount: BalanceOf<T>,
			delay: BlockNumberFor<T>,
		) -> Result<(), DispatchError> {
			let releasing_balance = Self::get_releasing_balance(account);
			T::Currency::set_freeze(
				&FreezeReason::Releasing.into(),
				account,
				releasing_balance.saturating_add(amount),
			)?;
			ReleaseQueues::<T>::try_mutate(account, |requests| -> DispatchResult {
				requests
					.try_push(ReleaseRequest {
						block: Self::current_block_number() + delay,
						amount,
					})
					.map_err(|_| Error::<T>::TooManyReleaseRequests)?;
				Ok(())
			})?;
			Ok(())
		}

		/// Distributes the rewards associated with a given collator, obtained during the previous session.
		/// This includes specific rewards for the collator plus rewards for the stakers.
		///
		/// The collator must be a candidate in order to receive the rewards.
		///
		/// Returns the number of rewarded stakers.
		fn do_reward_collator(
			collator: &T::AccountId,
			blocks: u32,
			session: SessionIndex,
		) -> (u32, u32) {
			let mut total_stakers = 0;
			let mut total_compound = 0;
			if Self::get_candidate(collator).is_ok() {
				let (_, rewardable_blocks) = TotalBlocks::<T>::get(session);
				// We cannot divide by zero.
				if rewardable_blocks.is_zero() {
					log::debug!(
						"Rewardable blocks is zero. Skipping rewards for collators and stakers..."
					);
					return (0, 0);
				}
				if blocks > rewardable_blocks {
					// The only case this could happen is if the candidate was an invulnerable during the session.
					log::warn!("Cannot reward collator {:?} for producing more blocks than rewardable ones", collator);
					return (0, 0);
				}

				if let Some(collator_info) = Candidates::<T>::get(collator) {
					let total_rewards = Rewards::<T>::get(session);
					let rewards_all: BalanceOf<T> =
						total_rewards.saturating_mul(blocks.into()) / rewardable_blocks.into();
					let collator_percentage = CollatorRewardPercentage::<T>::get();
					let collator_only_reward = collator_percentage.mul_floor(rewards_all);
					// Reward collator. Note these rewards are not autocompounded.
					if let Err(error) =
						Self::do_reward_single(collator, collator_only_reward, session)
					{
						log::warn!(target: LOG_TARGET, "Failure rewarding collator {:?}: {:?}", collator, error);
					}

					// Again, we cannot divide by zero.
					if collator_info.stake.is_zero() {
						log::debug!(
							"Candidate {:?} has no stakers. Skipping rewards for stakers...",
							collator
						);
						return (0, 0);
					}

					// Reward stakers
					let stakers_only_rewards = rewards_all.saturating_sub(collator_only_reward);
					CandidateStake::<T>::iter_prefix(collator).for_each(|(staker, info)| {
						if info.session >= session {
							// This staker joined during the session rewards are being distributed for.
							// No rewards for the staker for this session.
							return;
						}
						total_stakers += 1;
						let staker_reward: BalanceOf<T> =
							Perbill::from_rational(info.stake, collator_info.stake)
								.mul_floor(stakers_only_rewards);
						if let Err(error) = Self::do_reward_single(&staker, staker_reward, session)
						{
							log::warn!(target: LOG_TARGET, "Failure rewarding staker {:?}: {:?}", staker, error);
						} else {
							// AutoCompound
							total_compound += 1;
							let compound_percentage = AutoCompound::<T>::get(staker.clone());
							let compound_amount = compound_percentage.mul_floor(staker_reward);
							if !compound_amount.is_zero() {
								// We sort at the end, when the whole stake is included.
								if let Err(error) =
									Self::do_stake(&staker, collator, compound_amount)
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
				} else {
					log::warn!("Collator {:?} is no longer a candidate", collator);
				}
			}

			(total_stakers, total_compound)
		}

		fn do_reward_single(
			who: &T::AccountId,
			reward: BalanceOf<T>,
			session: SessionIndex,
		) -> DispatchResult {
			if !reward.is_zero() {
				T::Currency::transfer(&Self::account_id(), who, reward, Preserve)?;
				Self::deposit_event(Event::StakingRewardReceived {
					staker: who.clone(),
					amount: reward,
					session,
				});
			}
			Ok(())
		}

		/// Gets the current block number
		pub fn current_block_number() -> BlockNumberFor<T> {
			frame_system::Pallet::<T>::block_number()
		}

		/// Gets the locked balance potentially used for staking.
		pub fn get_staked_balance(account: &T::AccountId) -> BalanceOf<T> {
			T::Currency::balance_frozen(&FreezeReason::Staking.into(), account)
		}

		/// Gets the locked balance to be released.
		pub fn get_releasing_balance(account: &T::AccountId) -> BalanceOf<T> {
			T::Currency::balance_frozen(&FreezeReason::Releasing.into(), account)
		}

		/// Gets the locked balance for the candidacy bond.
		pub fn get_bond(account: &T::AccountId) -> BalanceOf<T> {
			T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), account)
		}

		/// Gets the maximum balance a given user can lock for staking.
		pub fn get_free_balance(account: &T::AccountId) -> BalanceOf<T> {
			T::Currency::balance(account)
				.saturating_sub(Self::get_staked_balance(account))
				.saturating_sub(Self::get_releasing_balance(account))
				.saturating_sub(Self::get_bond(account))
		}

		/// Assemble the current set of candidates and invulnerables into the next collator set.
		///
		/// This is done on the fly, as frequent as we are told to do so, as the session manager.
		pub fn assemble_collators() -> Vec<T::AccountId> {
			// Casting `u32` to `usize` should be safe on all machines running this.
			let desired_candidates = DesiredCandidates::<T>::get() as usize;
			let mut collators = Invulnerables::<T>::get().to_vec();
			let best_candidates = Self::get_sorted_candidate_list()
				.into_iter()
				.take(desired_candidates)
				.map(|(account, _)| account);
			collators.extend(best_candidates);
			collators
		}

		/// Gets the full list of candidates, sorted by stake.
		pub fn get_sorted_candidate_list() -> Vec<(T::AccountId, CandidateInfo<BalanceOf<T>>)> {
			let mut all_candidates = Candidates::<T>::iter().collect::<Vec<_>>();
			all_candidates.sort_by(|(_, info1), (_, info2)| info2.stake.cmp(&info1.stake));
			all_candidates
		}

		/// Kicks out candidates that did not produce a block in the kick threshold, and refunds
		/// the stakers. The candidate is refunded after a delay.
		///
		/// Return value is the number of candidates left in the list.
		pub fn kick_stale_candidates() -> u32 {
			let now = Self::current_block_number();
			let kick_threshold = T::KickThreshold::get();
			let min_collators = T::MinEligibleCollators::get();
			let candidacy_bond = MinCandidacyBond::<T>::get();
			Candidates::<T>::iter()
                .filter_map(|(who, info)| {
                    let last_block = LastAuthoredBlock::<T>::get(who.clone());
                    let since_last = now.saturating_sub(last_block);
                    let is_lazy = since_last >= kick_threshold;
                    let bond = Self::get_bond(&who);

                    if Self::eligible_collators() <= min_collators || (!is_lazy && bond.saturating_add(info.stake) >= candidacy_bond) {
                        // Either this is a good collator (not lazy) or we are at the minimum
                        // that the system needs. They get to stay, as long as they have sufficient deposit plus stake.
                        Some(info)
                    } else {
                        // This collator has not produced a block recently enough. Bye bye.
                        let _ = Self::try_remove_candidate(&who, true, true);
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
			if let Some((collator, blocks)) = ProducedBlocks::<T>::drain_prefix(session).next() {
				Self::do_reward_collator(&collator, blocks, session)
			} else {
				(0, 0)
			}
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

			ensure!(
				desired_candidates <= T::MaxCandidates::get(),
				"Shouldn't demand more candidates than the pallet config allows."
			);

			ensure!(
				desired_candidates.saturating_add(T::MaxInvulnerables::get()) >=
					T::MinEligibleCollators::get(),
				"Invulnerable set together with desired candidates should be able to meet the collator quota."
			);

			ensure!(
				UserStake::<T>::iter_values()
					.all(|UserStakeInfo { count, .. }| count < T::MaxStakedCandidates::get()),
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

	/// Impl of the session manager.
	impl<T: Config> SessionManager<T::AccountId> for Pallet<T> {
		fn new_session(index: SessionIndex) -> Option<Vec<T::AccountId>> {
			log::info!(
				target: LOG_TARGET,
				"assembling new collators for new session {} at #{:?}",
				index,
				Self::current_block_number(),
			);

			// The `expect` below is safe because the list is a `BoundedVec` with a max size of
			// `T::MaxCandidates`, which is a `u32`. When `decode_len` returns `Some(len)`, `len`
			// must be valid and at most `u32::MAX`, which must always be able to convert to `u32`.
			let candidates_len_before = Candidates::<T>::count();
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
			let total_rewards = T::Currency::reducible_balance(&pot_account, Preserve, Polite);
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

sp_api::decl_runtime_apis! {
	/// This runtime api allows people to query the two pot addresses.
	pub trait CollatorStakingApi<AccountId>
	where AccountId: Codec
	{
		/// Queries the main pot account
		fn main_pot_account() -> AccountId;

		/// Queries the extra reward pot account.
		fn extra_reward_pot_account() -> AccountId;
	}
}
