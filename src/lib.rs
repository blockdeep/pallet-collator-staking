// Copyright (C) BlockDeep Labs UG.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Collator Staking Pallet
//!
//! Simple DPoS pallet for staking and managing collators in a Polkadot parachain.
//!
//! ## Overview
//!
//! The Collator Staking pallet provides DPoS functionality to manage collators of a parachain.
//! It allows staking tokens to back collators, and receive rewards proportionately.
//! There is no slashing implemented. If a collator does not produce blocks as expected,
//! it is removed from the collator set.
//!
//! ## Reward distribution mechanism
//!
//! The pallet uses a checkpoint system to efficiently track and distribute rewards.
//! This allows automatically distribute rewards for autocompounding implementation.
//!
//! ### Example:
//! A collator has 3 stakers with 100 tokens each (total 300 tokens)
//! The collator's counter starts at 0
//! After a session with 30 tokens of rewards:
//!  * Counter increases by 30/300 = 0.1
//!  * Each token staked now "earned" 0.1 tokens
//!
//! A staker with 100 tokens claims rewards:
//!  * Unclaimed rewards = (0.1 - 0) * 100 = 10 tokens
//!  * Checkpoint updated from 0 to 0.1
//!
//! After another session with 60 tokens of rewards:
//!  * Counter increases by 60/300 = 0.2, now totaling 0.3
//!
//! The same staker claims again:
//!  * Unclaimed rewards = (0.3 - 0.1) * 100 = 20 tokens
//!  * Checkpoint updated from 0.1 to 0.3
//!
//! ## Two-Layer Auto-Compound Settings
//!
//! The pallet uses a two-layer approach for auto-compound distribution:
//! - Commit Layer: Active settings currently used for reward distribution.
//! - Staging Layer: Temporary storage for changes made during active distribution.

#![cfg_attr(not(feature = "std"), no_std)]
extern crate core;

use core::marker::PhantomData;

use frame_support::traits::TypedGet;
use sp_std::vec::Vec;

pub use api::*;
pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

pub mod api;
#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;
pub mod migrations;
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
		weights::WeightMeter,
		BoundedVec, DefaultNoBound, PalletId,
	};
	use frame_system::pallet_prelude::*;
	use pallet_session::SessionManager;
	use sp_runtime::{
		traits::{AccountIdConversion, Convert, Saturating, Zero},
		FixedPointNumber, FixedU128, Perbill, Percent, RuntimeDebug,
	};
	use sp_staking::SessionIndex;

	pub use crate::weights::WeightInfo;

	use super::*;

	/// The in-code storage version.
	const STORAGE_VERSION: StorageVersion = StorageVersion::new(2);

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;
	pub type SessionInfoOf<T> = SessionInfo<
		BoundedBTreeMap<
			<T as frame_system::Config>::AccountId,
			(BalanceOf<T>, BalanceOf<T>), // first item is the stake and second one the rewards generated.
			<T as Config>::MaxCandidates,
		>,
		BalanceOf<T>,
	>;
	pub type CandidateInfoOf<T> = CandidateInfo<BalanceOf<T>>;
	pub type ReleaseRequestOf<T> = ReleaseRequest<BlockNumberFor<T>, BalanceOf<T>>;
	pub type StakeTargetOf<T> = StakeTarget<<T as frame_system::Config>::AccountId, BalanceOf<T>>;
	pub type UserStakeInfoOf<T> = UserStakeInfo<
		BoundedBTreeSet<<T as frame_system::Config>::AccountId, <T as Config>::MaxStakedCandidates>,
		BalanceOf<T>,
		BlockNumberFor<T>,
	>;
	pub type CandidateStakeInfoOf<T> = CandidateStakeInfo<BalanceOf<T>>;
	pub type CandidacyBondReleaseOf<T> = CandidacyBondRelease<BalanceOf<T>, BlockNumberFor<T>>;
	pub type OperationFor<T> = Operation<<T as frame_system::Config>::AccountId>;

	/// A convertor from collators id. Since this pallet does not have stash/controller, this is
	/// just identity.
	pub struct IdentityCollator;

	impl<T> Convert<T, Option<T>> for IdentityCollator {
		fn convert(t: T) -> Option<T> {
			Some(t)
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

		/// Candidates will be removed from active collator set, if block is not produced within this threshold.
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

		/// Number of blocks to wait before reusing funds previously assigned to a candidate.
		/// It should be set to at least one session.
		#[pallet::constant]
		type RestakeUnlockDelay: Get<BlockNumberFor<Self>>;

		/// Minimum stake needed to enable autocompounding.
		#[pallet::constant]
		type AutoCompoundingThreshold: Get<BalanceOf<Self>>;

		/// The weight information of this pallet.
		type WeightInfo: WeightInfo;
	}

	/// A reason for the pallet to freeze funds.
	#[pallet::composite_enum]
	pub enum FreezeReason {
		Staking,
		#[deprecated]
		CandidacyBond,
		#[deprecated]
		Releasing,
	}

	/// Basic information about a candidate.
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

	#[derive(
		PartialEq,
		Eq,
		Clone,
		Encode,
		Decode,
		DecodeWithMemTracking,
		RuntimeDebug,
		scale_info::TypeInfo,
		MaxEncodedLen,
	)]
	pub struct StakeTarget<AccountId, Balance> {
		pub candidate: AccountId,
		pub stake: Balance,
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
		/// The amount staked.
		pub stake: Balance,
		/// Checkpoint to track rewards for a given collator.
		/// Represents the last point at which a staker was given rewards.
		pub checkpoint: FixedU128,
	}

	/// Information about a users' stake.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct UserStakeInfo<AccountIdSet, Balance, BlockNumber> {
		/// The total amount staked in all candidates.
		pub stake: Balance,
		/// Last time an amount was reassigned.
		pub maybe_last_unstake: Option<(Balance, BlockNumber)>,
		/// The candidates where this user staked.
		pub candidates: AccountIdSet,
		/// Last session where this user got the rewards.
		pub maybe_last_reward_session: Option<SessionIndex>,
	}

	impl<AccountIdSet, Balance, BlockNumber> Default
		for UserStakeInfo<AccountIdSet, Balance, BlockNumber>
	where
		AccountIdSet: Default,
		Balance: Default,
	{
		fn default() -> Self {
			Self {
				stake: Balance::default(),
				candidates: AccountIdSet::default(),
				maybe_last_unstake: None,
				maybe_last_reward_session: None,
			}
		}
	}

	/// Information about a session rewards.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct SessionInfo<AccountIdMap, Balance> {
		/// The total rewards generated during the session.
		pub rewards: Balance,
		/// Total rewards already claimed by stakers. It must be lower than or equal to `rewards`.
		pub claimed_rewards: Balance,
		/// The candidates that participated in this session.
		pub candidates: AccountIdMap,
	}

	/// Reasons a candidacy left the candidacy list for.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub enum CandidacyBondReleaseReason {
		/// The candidacy did not produce at least one block for [`KickThreshold`] blocks.
		Idle,
		/// The candidate left by itself.
		Left,
		/// The candidate was replaced by another one with higher bond.
		Replaced,
	}

	/// The storage layers.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub enum Layer {
		/// Changes yet to be commited.
		Staging,
		/// Changes already being applied.
		Commit,
	}

	/// The storage layers.
	#[derive(
		PartialEq,
		Eq,
		Clone,
		Encode,
		Decode,
		RuntimeDebug,
		scale_info::TypeInfo,
		MaxEncodedLen,
		Default,
	)]
	pub enum Operation<AccountId> {
		/// Nothing else to do.
		#[default]
		Idle,
		/// Tracks the rewards to be delivered by the system.
		RewardStakers { maybe_last_processed_account: Option<AccountId> },
		/// Tracks the process of migration from the Staging to the Commit layers in Autocompounding.
		CommitAutocompound,
	}

	/// Represents a bond release for a collator candidacy.
	///
	/// This struct encapsulates crucial information regarding the release of a bond tied to a
	/// collator's candidacy. The bond can only be released after a specified block number and for a
	/// concrete reason defined in `CandidacyBondReleaseReason`.
	#[derive(
		PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
	)]
	pub struct CandidacyBondRelease<Balance, BlockNumber> {
		/// The amount of bond associated with the candidacy.
		pub bond: Balance,
		/// The block number when the bond can be released.
		pub block: BlockNumber,
		/// The reason for the bond release, represented by [`CandidacyBondReleaseReason`].
		pub reason: CandidacyBondReleaseReason,
	}

	/// Tracks the different types of locks that can be applied to an account's balance.
	///
	/// This struct keeps track of the different amounts that are locked for staking,
	/// releasing (in process of being unlocked), and held as candidacy bond.
	#[derive(
		PartialEq,
		Eq,
		Clone,
		Encode,
		Decode,
		RuntimeDebug,
		scale_info::TypeInfo,
		MaxEncodedLen,
		Default,
	)]
	pub struct LockedBalance<Balance> {
		/// The amount that is in the process of being released.
		pub releasing: Balance,
		/// The amount locked as candidacy bond.
		pub candidacy_bond: Balance,
	}

	impl<Balance> LockedBalance<Balance>
	where
		Balance: Saturating + Copy,
	{
		/// Returns the total amount of locked balance that is not part of staking.
		pub fn total(&self) -> Balance {
			self.releasing.saturating_add(self.candidacy_bond)
		}
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
	#[pallet::storage]
	pub type Candidates<T: Config> =
		CountedStorageMap<_, Blake2_128Concat, T::AccountId, CandidateInfoOf<T>, OptionQuery>;

	/// Map of Candidates that have been removed in the current session.
	#[pallet::storage]
	pub type SessionRemovedCandidates<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, CandidateInfoOf<T>, OptionQuery>;

	/// Last block authored by a collator.
	#[pallet::storage]
	pub type LastAuthoredBlock<T: Config> =
		StorageMap<_, Twox64Concat, T::AccountId, BlockNumberFor<T>, ValueQuery>;

	/// Desired number of candidates.
	///
	/// This should always be less than [`Config::MaxCandidates`] for weights to be correct.
	#[pallet::storage]
	pub type DesiredCandidates<T> = StorageValue<_, u32, ValueQuery>;

	/// Minimum amount to become a candidate.
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
		CandidateStakeInfoOf<T>,
		ValueQuery,
	>;

	/// Number of candidates staked on by a user.
	///
	/// Cannot be higher than `MaxStakedCandidates`.
	#[pallet::storage]
	pub type UserStake<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, UserStakeInfoOf<T>, ValueQuery>;

	/// Release requests for an account.
	///
	/// They can be actually released by calling the [`release`] extrinsic, after the relevant delay.
	#[pallet::storage]
	pub type ReleaseQueues<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		T::AccountId,
		BoundedVec<ReleaseRequestOf<T>, T::MaxStakedCandidates>,
		ValueQuery,
	>;

	/// Percentage of rewards that would go for collators.
	#[pallet::storage]
	pub type CollatorRewardPercentage<T: Config> = StorageValue<_, Percent, ValueQuery>;

	/// Per-block extra reward.
	#[pallet::storage]
	pub type ExtraReward<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Blocks produced in the current session. First value is the total,
	/// and second is blocks produced by candidates only (not invulnerables).
	#[pallet::storage]
	pub type TotalBlocks<T: Config> = StorageValue<_, (u32, u32), ValueQuery>;

	/// Mapping of blocks and their authors.
	#[pallet::storage]
	pub type ProducedBlocks<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, u32, ValueQuery>;

	/// Current session index. Obtained from `pallet-session`.
	#[pallet::storage]
	pub type CurrentSession<T: Config> = StorageValue<_, SessionIndex, ValueQuery>;

	/// Claimable rewards. This represents the portion of the main pallet's pot account that belong
	/// to user rewards. The rest of the funds are those generated during the current session that
	/// will become actual rewards when the session ends.
	#[pallet::storage]
	pub type ClaimableRewards<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

	/// Keeps track of whether auto-compound rewards are enabled for an account
	/// in a specific layer.
	#[pallet::storage]
	pub type AutoCompoundSettings<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		Layer,
		Blake2_128Concat,
		T::AccountId,
		bool,
		ValueQuery,
	>;

	/// Time (in blocks) to release an ex-candidate's locked candidacy bond.
	/// If a candidate leaves the candidacy before its bond is released, the waiting period
	/// will restart.
	#[pallet::storage]
	pub type CandidacyBondReleases<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, CandidacyBondReleaseOf<T>, OptionQuery>;

	/// Represents accumulated rewards per token staked on a given collator over time.
	#[pallet::storage]
	pub type Counters<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, FixedU128, ValueQuery>;

	/// The storage value `LastRewardedKey` is used to track the last key that was rewarded
	/// automatically by the system.
	///
	/// Only those accounts with autocompound enabled have their rewards automatically collected.
	///
	/// - `bool`: Indicates if reward distribution was completed for the current session.
	/// - `Option<T::AccountId>`: The account ID of the last rewarded key, if any.
	#[pallet::storage]
	pub type NextSystemOperation<T: Config> = StorageValue<_, OperationFor<T>, ValueQuery>;

	/// Tracks the different types of locked balances for each account.
	///
	/// This storage map keeps track of how much of an account's locked balance is:
	/// - In the process of being released via the release queue
	/// - Locked as a candidacy bond
	///
	/// The total locked balance for staking purposes can be calculated as:
	/// `total_frozen - (releasing + candidacy_bond)`
	#[pallet::storage]
	pub type LockedBalances<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, LockedBalance<BalanceOf<T>>, ValueQuery>;

	#[pallet::genesis_config]
	#[derive(DefaultNoBound)]
	pub struct GenesisConfig<T: Config> {
		pub invulnerables: Vec<T::AccountId>,
		pub min_candidacy_bond: BalanceOf<T>,
		pub min_stake: BalanceOf<T>,
		pub desired_candidates: u32,
		pub collator_reward_percentage: Percent,
		pub extra_reward: BalanceOf<T>,
	}

	#[pallet::genesis_build]
	impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
		fn build(&self) {
			assert!(
				!Pallet::<T>::has_duplicates(&self.invulnerables),
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

			DesiredCandidates::<T>::set(self.desired_candidates);
			MinCandidacyBond::<T>::set(self.min_candidacy_bond);
			MinStake::<T>::set(self.min_stake);
			Invulnerables::<T>::set(bounded_invulnerables);
			CollatorRewardPercentage::<T>::set(self.collator_reward_percentage);
			ExtraReward::<T>::set(self.extra_reward);
		}
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub (super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// New Invulnerables were set.
		NewInvulnerables { invulnerables: Vec<T::AccountId> },
		/// A new Invulnerable was added.
		InvulnerableAdded { account: T::AccountId },
		/// An Invulnerable was removed.
		InvulnerableRemoved { account_id: T::AccountId },
		/// The number of desired candidates was set.
		NewDesiredCandidates { desired_candidates: u32 },
		/// The minimum candidacy bond was set.
		NewMinCandidacyBond { bond_amount: BalanceOf<T> },
		/// A new candidate joined.
		CandidateAdded { account: T::AccountId, deposit: BalanceOf<T> },
		/// A candidate was removed.
		CandidateRemoved { account: T::AccountId },
		/// An account was unable to be added to the Invulnerables because they did not have keys
		/// registered. Other Invulnerables may have been set.
		InvalidInvulnerableSkipped { account: T::AccountId },
		/// A staker added stake to a candidate.
		StakeAdded { account: T::AccountId, candidate: T::AccountId, amount: BalanceOf<T> },
		/// Stake was claimed after a penalty period.
		StakeReleased { account: T::AccountId, amount: BalanceOf<T> },
		/// An unstake request was created.
		ReleaseRequestCreated {
			account: T::AccountId,
			amount: BalanceOf<T>,
			block: BlockNumberFor<T>,
		},
		/// A staker removed stake from a candidate
		StakeRemoved { account: T::AccountId, candidate: T::AccountId, amount: BalanceOf<T> },
		/// A staking reward was delivered.
		StakingRewardReceived { account: T::AccountId, amount: BalanceOf<T> },
		/// Autocompounding was enabled.
		AutoCompoundEnabled { account: T::AccountId },
		/// Autocompounding was disabled.
		AutoCompoundDisabled { account: T::AccountId },
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
		LockExtended { account: T::AccountId, amount: BalanceOf<T> },
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
		/// At least one of the invulnerables is duplicated
		DuplicatedInvulnerables,
		/// Account is already an Invulnerable.
		AlreadyInvulnerable,
		/// Account is not an Invulnerable.
		NotInvulnerable,
		/// Account has no associated validator ID.
		NoAssociatedCollatorId,
		/// Collator ID is not yet registered.
		CollatorNotRegistered,
		/// Amount not sufficient to be staked.
		InsufficientStake,
		/// DesiredCandidates is out of bounds.
		TooManyDesiredCandidates,
		/// Too many unstaking requests. Claim some of them first.
		TooManyReleaseRequests,
		/// Invalid value for MinStake. It must be lower than or equal to [`MinStake`].
		InvalidMinStake,
		/// Invalid value for CandidacyBond. It must be higher than or equal to [`MinCandidacyBond`].
		InvalidCandidacyBond,
		/// Number of staked candidates is greater than [`MaxStakedCandidates`].
		TooManyStakedCandidates,
		/// Extra reward cannot be zero.
		InvalidExtraReward,
		/// Extra rewards are already zero.
		ExtraRewardAlreadyDisabled,
		/// The amount to fund the extra reward pot must be greater than zero.
		InvalidFundingAmount,
		/// Cannot add more stakers to a given candidate.
		TooManyStakers,
		/// The user does not have enough balance to be locked for staking.
		InsufficientFreeBalance,
		/// The user does not have enough locked balance to stake.
		InsufficientLockedBalance,
		/// Cannot unlock such amount.
		CannotUnlock,
		/// User must stake at least on one candidate.
		TooFewCandidates,
		/// Rewards from previous sessions have not yet been claimed.
		PreviousRewardsNotClaimed,
		/// User has not Staked on the given Candidate.
		NoStakeOnCandidate,
		/// No rewards to claim as previous claim happened on the same session.
		NoPendingClaim,
		/// Candidate has not been removed in the current session.
		NotRemovedCandidate,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn integrity_test() {
			assert!(T::MinEligibleCollators::get() > 0, "chain must require at least one collator");
			assert!(
				T::MaxCandidates::get() >= T::MaxStakedCandidates::get(),
				"MaxCandidates must be greater than or equal to MaxStakedCandidates"
			);
		}

		#[cfg(feature = "try-runtime")]
		fn try_state(_: BlockNumberFor<T>) -> Result<(), sp_runtime::TryRuntimeError> {
			Self::do_try_state()
		}

		/// Performs operations in a loop based on the current state of the [`NextSystemOperation`].
		/// Specifically, it processes rewards for stakers with auto-compound enabled and commits
		/// auto-compound operations to the appropriate storage.
		fn on_idle(_n: BlockNumberFor<T>, remaining_weight: Weight) -> Weight {
			let mut meter = WeightMeter::with_limit(remaining_weight);
			if meter.try_consume(T::DbWeight::get().reads_writes(1, 0)).is_err() {
				return remaining_weight;
			}
			let mut next_operation = NextSystemOperation::<T>::get();
			loop {
				next_operation = match next_operation {
					// Step 1: Reward stakers with autocompound enabled.
					Operation::RewardStakers { maybe_last_processed_account } => {
						match Self::do_reward_stakers(&mut meter, maybe_last_processed_account) {
							Ok(op) => op,
							Err(_) => break,
						}
					},
					// Step 2: move staging operations to the commit layer.
					Operation::CommitAutocompound => match Self::do_commit_autocompound(&mut meter)
					{
						Ok(op) => op,
						Err(_) => break,
					},
					// Nothing to do here.
					Operation::Idle => break,
				}
			}
			meter.consumed()
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Set the list of invulnerable (fixed) collators. These collators must:
		///   - Have registered session keys.
		///   - Not currently be collator candidates (the call will fail if an entry is already a candidate).
		///
		/// If the provided list is empty, it also ensures that the total number of eligible collators
		/// does not fall below the configured minimum.
		///
		/// This call does not inherently maintain mutual exclusivity with `Candidates`, but in practice,
		/// accounts that are already candidates will be rejected. If you need to convert a candidate
		/// to be invulnerable, remove them from the set of candidates first, then call this function.
		///
		/// Must be called by the `UpdateOrigin`.
		#[pallet::call_index(0)]
		#[pallet::weight(T::WeightInfo::set_invulnerables(new.len() as u32))]
		pub fn set_invulnerables(origin: OriginFor<T>, new: Vec<T::AccountId>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;

			ensure!(!Self::has_duplicates(&new), Error::<T>::DuplicatedInvulnerables);

			// Don't wipe out the collator set
			if new.is_empty() {
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
								account: account_id.clone(),
							});
							continue;
						}
						// else condition passes; key is registered
					},
					// key does not exist
					None => {
						Self::deposit_event(Event::InvalidInvulnerableSkipped {
							account: account_id.clone(),
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

			// Make sure that the minimum eligible collator requirement is met.
			let total_invulnerables = bounded_invulnerables.len() as u32;
			let eligible_collators = total_invulnerables.saturating_add(Candidates::<T>::count());
			ensure!(
				eligible_collators >= T::MinEligibleCollators::get(),
				Error::<T>::TooFewEligibleCollators
			);

			// Invulnerables must be sorted for removal.
			bounded_invulnerables.sort();

			let invulnerables = bounded_invulnerables.to_vec();
			Invulnerables::<T>::set(bounded_invulnerables);
			Self::deposit_event(Event::NewInvulnerables { invulnerables });

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
			ensure!(max <= T::MaxCandidates::get(), Error::<T>::TooManyDesiredCandidates);

			let invulnerables = Invulnerables::<T>::get();
			ensure!(
				max.saturating_add(invulnerables.len() as u32) >= T::MinEligibleCollators::get(),
				Error::<T>::TooFewEligibleCollators
			);

			DesiredCandidates::<T>::set(max);
			Self::deposit_event(Event::NewDesiredCandidates { desired_candidates: max });
			Ok(())
		}

		/// Set the candidacy bond amount, which represents the required amount to reserve for an
		/// account to become a candidate. The candidacy bond does not count as stake.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::set_min_candidacy_bond())]
		pub fn set_min_candidacy_bond(origin: OriginFor<T>, bond: BalanceOf<T>) -> DispatchResult {
			T::UpdateOrigin::ensure_origin(origin)?;
			MinCandidacyBond::<T>::set(bond);
			Self::deposit_event(Event::NewMinCandidacyBond { bond_amount: bond });
			Ok(())
		}

		/// Register this account as a collator candidate. The account must (a) already have
		/// registered session keys and (b) be able to reserve the `CandidacyBond`.
		/// The `CandidacyBond` amount is automatically reserved from the balance of the caller.
		///
		/// This call is not available to `Invulnerable` collators.
		#[pallet::call_index(3)]
		#[pallet::weight(
			T::WeightInfo::register_as_candidate()
				.saturating_add(T::WeightInfo::remove_worst_candidate())
		)]
		pub fn register_as_candidate(
			origin: OriginFor<T>,
			bond: BalanceOf<T>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;

			let length = Candidates::<T>::count();
			ensure!(!Self::is_invulnerable(&who), Error::<T>::AlreadyInvulnerable);

			let validator_key =
				T::CollatorIdOf::convert(who.clone()).ok_or(Error::<T>::NoAssociatedCollatorId)?;
			ensure!(
				T::CollatorRegistration::is_registered(&validator_key),
				Error::<T>::CollatorNotRegistered
			);

			let mut weight = T::WeightInfo::register_as_candidate();
			if length >= T::MaxCandidates::get() {
				// We have too many candidates, so we have to remove the one with the lowest
				// candidacy bond.
				Self::remove_worst_candidate(bond)?;
				weight.saturating_accrue(T::WeightInfo::remove_worst_candidate());
			}

			Self::do_register_as_candidate(&who, bond)?;
			Ok(Some(weight).into())
		}

		/// Deregister `origin` as a collator candidate. No rewards will be delivered to this
		/// candidate and its stakers after this moment.
		///
		/// This call will fail if the total number of candidates would drop below `MinEligibleCollators`.
		#[pallet::call_index(4)]
		#[pallet::weight(T::WeightInfo::leave_intent())]
		pub fn leave_intent(origin: OriginFor<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;
			ensure!(
				Self::eligible_collators() > T::MinEligibleCollators::get(),
				Error::<T>::TooFewEligibleCollators
			);
			// Do remove their last authored block.
			Self::try_remove_candidate(&who, true, CandidacyBondReleaseReason::Left)?;

			Ok(())
		}

		/// Add a new account `who` to the list of `Invulnerables` collators. `who` must have
		/// registered session keys. If `who` is a candidate, the operation will be aborted.
		///
		/// The origin for this call must be the `UpdateOrigin`.
		#[pallet::call_index(5)]
		#[pallet::weight(T::WeightInfo::add_invulnerable(
        T::MaxInvulnerables::get().saturating_sub(1),
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

			Self::deposit_event(Event::InvulnerableAdded { account: who });

			let weight_used = T::WeightInfo::add_invulnerable(
				Invulnerables::<T>::decode_len()
					.unwrap_or_default()
					.try_into()
					.unwrap_or(T::MaxInvulnerables::get().saturating_sub(1)),
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

		/// Allows a user to stake on a set of collator candidates.
		///
		/// The call will fail if:
		///     - `origin` does not have the at least [`MinStake`] deposited in the candidate.
		///     - one of the `targets` is not in the [`Candidates`] map.
		///     - the user does not have sufficient locked balance to stake.
		///     - zero targets are passed.
		#[pallet::call_index(7)]
		#[pallet::weight(T::WeightInfo::stake(targets.len() as u32))]
		pub fn stake(
			origin: OriginFor<T>,
			targets: BoundedVec<StakeTargetOf<T>, T::MaxStakedCandidates>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let len = targets.len() as u32;
			ensure!(len > 0, Error::<T>::TooFewCandidates);

			ensure!(Self::staker_has_claimed(&who), Error::<T>::PreviousRewardsNotClaimed);

			for StakeTarget { candidate, stake } in targets {
				Self::do_stake(&who, &candidate, stake)?;
			}
			Ok(Some(T::WeightInfo::stake(len)).into())
		}

		/// Removes stake from a collator candidate.
		///
		/// The amount unstaked will remain locked if the stake was removed from a candidate.
		#[pallet::call_index(8)]
		#[pallet::weight(T::WeightInfo::unstake_from())]
		pub fn unstake_from(origin: OriginFor<T>, account: T::AccountId) -> DispatchResult {
			let who = ensure_signed(origin)?;

			ensure!(Self::staker_has_claimed(&who), Error::<T>::PreviousRewardsNotClaimed);

			ensure!(
				CandidateStake::<T>::try_get(account.clone(), who.clone()).is_ok(),
				Error::<T>::NoStakeOnCandidate
			);

			let (amount, is_candidate) = Self::do_unstake(&who, &account)?;
			if is_candidate {
				Self::note_last_unstake(&who, amount);
			}

			Ok(())
		}

		/// Removes all stake of a user from all candidates.
		///
		/// The amount unstaked from candidates will remain locked.
		#[pallet::call_index(9)]
		#[pallet::weight(T::WeightInfo::unstake_all(T::MaxStakedCandidates::get()))]
		pub fn unstake_all(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;

			ensure!(Self::staker_has_claimed(&who), Error::<T>::PreviousRewardsNotClaimed);

			let user_stake = UserStake::<T>::get(&who);
			let mut amount_in_candidates: BalanceOf<T> = Zero::zero();
			for candidate in &user_stake.candidates {
				let (amount, is_candidate) = Self::do_unstake(&who, candidate)?;
				if is_candidate {
					amount_in_candidates.saturating_accrue(amount);
				}
			}
			if !amount_in_candidates.is_zero() {
				Self::note_last_unstake(&who, amount_in_candidates);
			}

			Ok(Some(T::WeightInfo::unstake_all(user_stake.candidates.len() as u32)).into())
		}

		/// Releases all pending [`ReleaseRequest`] and candidacy bond for a given account.
		///
		/// This will unlock all funds in [`ReleaseRequest`] that have already expired.
		#[pallet::call_index(10)]
		#[pallet::weight(T::WeightInfo::release(T::MaxStakedCandidates::get()))]
		pub fn release(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			Self::do_claim_candidacy_bond(&who)?;
			let operations = Self::do_release(&who)?;
			Ok(Some(T::WeightInfo::release(operations)).into())
		}

		/// Sets the percentage of rewards that should be auto-compounded.
		///
		/// This operation will also claim all pending rewards.
		/// Rewards will be autocompounded when calling the `claim_rewards` extrinsic.
		#[pallet::call_index(11)]
		#[pallet::weight(T::WeightInfo::set_autocompound())]
		pub fn set_autocompound(origin: OriginFor<T>, enable: bool) -> DispatchResult {
			let who = ensure_signed(origin)?;

			ensure!(Self::staker_has_claimed(&who), Error::<T>::PreviousRewardsNotClaimed);

			Self::do_set_autocompound(&who, enable)?;

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

			CollatorRewardPercentage::<T>::set(percent);
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

			ExtraReward::<T>::set(extra_reward);
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

			MinStake::<T>::set(new_min_stake);
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
			let maybe_receiver = T::ExtraRewardReceiver::get();
			if !balance.is_zero() {
				if let Some(ref receiver) = maybe_receiver {
					if let Err(error) = T::Currency::transfer(&pot, receiver, balance, Expendable) {
						// We should not cancel the operation if we cannot transfer funds from the pot,
						// as it is more important to stop the rewards.
						log::warn!("Failure transferring extra reward pot remaining balance to the destination account {:?}: {:?}", receiver, error);
					}
				}
			}
			Self::deposit_event(Event::ExtraRewardRemoved {
				amount_left: balance,
				receiver: maybe_receiver,
			});
			Ok(())
		}

		/// Transfers funds to the extra reward pot account for distribution.
		///
		/// **Parameters**:
		/// - `origin`: Signed account initiating the transfer.
		/// - `amount`: Amount to transfer.
		///
		/// **Errors**:
		/// - `Error::<T>::InvalidFundingAmount`: Amount is zero.
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

		/// Locks free balance from the caller to be used for staking.
		///
		/// **Parameters**:
		/// - `origin`: Signed account initiating the lock.
		/// - `amount`: Amount to lock.
		///
		/// **Errors**:
		/// - `Error::<T>::InvalidFundingAmount`: Amount is zero.
		#[pallet::call_index(17)]
		#[pallet::weight(T::WeightInfo::lock())]
		pub fn lock(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;
			ensure!(!amount.is_zero(), Error::<T>::InvalidFundingAmount);

			Self::do_lock(&who, amount)
		}

		/// Adds staked funds to the [`ReleaseRequest`] queue.
		///
		/// Funds will actually be released after [`StakeUnlockDelay`].
		#[pallet::call_index(18)]
		#[pallet::weight(T::WeightInfo::unlock())]
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
			Self::add_to_release_queue(&who, amount, T::StakeUnlockDelay::get())?;
			Self::adjust_autocompound(&who);

			Ok(())
		}

		/// Updates the candidacy bond for this candidate.
		///
		/// For this operation to succeed, the caller must:
		///   - Be a candidate.
		///   - Have sufficient free balance to be locked.
		#[pallet::call_index(19)]
		#[pallet::weight(T::WeightInfo::update_candidacy_bond())]
		pub fn update_candidacy_bond(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
			let who = ensure_signed(origin)?;
			ensure!(amount >= MinCandidacyBond::<T>::get(), Error::<T>::InvalidCandidacyBond);
			ensure!(Self::get_candidate(&who).is_ok(), Error::<T>::NotCandidate);

			LockedBalances::<T>::try_mutate(&who, |lock| -> DispatchResult {
				if lock.candidacy_bond < amount {
					Self::increase_frozen(&who, amount.saturating_sub(lock.candidacy_bond))?;
				} else {
					Self::decrease_frozen(&who, lock.candidacy_bond.saturating_sub(amount))?;
				}
				lock.candidacy_bond = amount;
				Ok(())
			})?;

			Self::deposit_event(Event::<T>::CandidacyBondUpdated {
				candidate: who,
				new_bond: amount,
			});

			Ok(())
		}

		/// Claims all pending rewards for a given staker.
		///
		/// Distributes rewards accumulated over previous sessions.
		/// Rewards for the current session cannot be claimed.
		///
		/// **Errors**:
		/// - `Error::<T>::NoPendingClaim`: Caller has no rewards to claim.
		#[pallet::call_index(20)]
		#[pallet::weight(T::WeightInfo::claim_rewards(T::MaxStakedCandidates::get()))]
		pub fn claim_rewards(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;

			// Staker can't claim in the same session as there are no rewards.
			ensure!(!Self::staker_has_claimed(&who), Error::<T>::NoPendingClaim);

			let candidates = Self::do_claim_rewards(&who)?;
			Ok(Some(T::WeightInfo::claim_rewards(candidates)).into())
		}

		/// Claims all pending rewards for `target`.
		///
		/// Distributes rewards accumulated over previous sessions.
		/// Rewards for the current session cannot be claimed.
		///
		/// **Errors**:
		/// - `Error::<T>::NoPendingClaim`: `target` has no rewards to claim.
		#[pallet::call_index(21)]
		#[pallet::weight(T::WeightInfo::claim_rewards(T::MaxStakedCandidates::get()))]
		pub fn claim_rewards_other(
			origin: OriginFor<T>,
			target: T::AccountId,
		) -> DispatchResultWithPostInfo {
			// We do not care about the sender.
			ensure_signed(origin)?;

			// Staker can't claim in the same session as there are no rewards.
			ensure!(!Self::staker_has_claimed(&target), Error::<T>::NoPendingClaim);

			let candidates = Self::do_claim_rewards(&target)?;
			Ok(Some(T::WeightInfo::claim_rewards(candidates)).into())
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

		/// Checks whether a given `account` is a candidate and returns its position if successful.
		pub fn get_candidate(account: &T::AccountId) -> Result<CandidateInfoOf<T>, DispatchError> {
			Candidates::<T>::get(account).ok_or(Error::<T>::NotCandidate.into())
		}

		/// Checks whether a given `account` is an invulnerable.
		pub fn is_invulnerable(account: &T::AccountId) -> bool {
			Invulnerables::<T>::get().binary_search(account).is_ok()
		}

		/// Checks if the provided list of accounts contains duplicate entries.
		fn has_duplicates(accounts: &[T::AccountId]) -> bool {
			let duplicates =
				accounts.iter().collect::<sp_std::collections::btree_set::BTreeSet<_>>();
			duplicates.len() != accounts.len()
		}

		/// Processes staking rewards for stakers who have enabled auto-compounding,
		/// while respecting weight limits. It iterates through the stakers, claims rewards,
		/// and tracks the last processed account for continuation in subsequent executions.
		/// If all accounts are processed, it sets the next system operation to commit auto-compounding.
		pub(crate) fn do_reward_stakers(
			meter: &mut WeightMeter,
			mut maybe_last_processed_account: Option<T::AccountId>,
		) -> Result<OperationFor<T>, ()> {
			// This is the weight of the final operation in this function where we set
			// `NextSystemOperation`.
			let write = T::DbWeight::get().reads_writes(0, 1);
			if meter.try_consume(write).is_err() {
				return Err(());
			}
			let worst_case_weight = T::WeightInfo::claim_rewards(T::MaxStakedCandidates::get());
			let mut iter = if let Some(last_key) = &maybe_last_processed_account {
				let key = AutoCompoundSettings::<T>::hashed_key_for(Layer::Commit, last_key);
				AutoCompoundSettings::<T>::iter_prefix_from(Layer::Commit, key)
			} else {
				AutoCompoundSettings::<T>::iter_prefix(Layer::Commit)
			};

			let read = T::DbWeight::get().reads_writes(1, 0);
			while meter.can_consume(worst_case_weight.saturating_add(read)) {
				if let Some((staker, enabled)) = iter.next() {
					meter.consume(read);
					if enabled {
						match Self::do_claim_rewards(&staker) {
							Ok(explored_candidates) => {
								meter.consume(T::WeightInfo::claim_rewards(explored_candidates));
							},
							Err(e) => {
								meter.consume(worst_case_weight);
								log::warn!("Error while attempting to collect rewards for staker {:?}: {:?}", staker, e);
							},
						};
					}
					maybe_last_processed_account = Some(staker);
				} else {
					// End of the iteration.
					meter.consume(T::DbWeight::get().reads_writes(1, 1));
					maybe_last_processed_account = None;
					break;
				}
			}
			if let Some(last_key) = maybe_last_processed_account {
				// In this branch we did not manage to finish traversing the whole map, so we save
				// the progress and return an error.
				NextSystemOperation::<T>::set(Operation::RewardStakers {
					maybe_last_processed_account: Some(last_key),
				});
				Err(())
			} else {
				// Managed to finish iterating the map, so we save the progress and continue with
				// the next step.
				let op = Operation::CommitAutocompound;
				NextSystemOperation::<T>::set(op.clone());
				Ok(op)
			}
		}

		/// Commits the auto-compounding options for stakers.
		///
		/// This function processes a batch of stakers' auto-compounding preferences
		/// from the staging layer and commits them to the active layer while consuming
		/// weight for each processed item. If the iteration completes, it sets the next
		/// system operation to `Idle`.
		pub(crate) fn do_commit_autocompound(
			meter: &mut WeightMeter,
		) -> Result<OperationFor<T>, ()> {
			let mut iter = AutoCompoundSettings::<T>::drain_prefix(Layer::Staging);
			let worst_case_weight = T::DbWeight::get().reads_writes(1, 2);
			while meter.try_consume(worst_case_weight).is_ok() {
				if let Some((staker, enabled)) = iter.next() {
					if enabled {
						AutoCompoundSettings::<T>::set(Layer::Commit, &staker, true);
					} else {
						AutoCompoundSettings::<T>::remove(Layer::Commit, &staker);
					}
				} else {
					let op = Operation::Idle;
					NextSystemOperation::<T>::set(op.clone());
					return Ok(op);
				}
			}
			Err(())
		}

		/// Claims staking rewards for the provided account.
		///
		/// - Iterates over all candidates the account has staked and calculates rewards.
		/// - Updates reward-related states such as `checkpoint` and `maybe_last_reward_session`.
		/// - Transfers rewards to the account. If auto-compounding is enabled, locks and stakes
		///   the rewards with the respective candidates.
		///
		/// Returns:
		/// - The number of candidates whose rewards were claimed.
		pub(crate) fn do_claim_rewards(who: &T::AccountId) -> Result<u32, DispatchError> {
			let mut total_rewards: BalanceOf<T> = Zero::zero();
			let mut candidate_rewards = Vec::new();
			let current_session = CurrentSession::<T>::get();
			UserStake::<T>::try_mutate(who, |user_stake_info| {
				for candidate in &user_stake_info.candidates {
					let counter = Counters::<T>::get(candidate);
					CandidateStake::<T>::mutate(candidate, who, |info| {
						let reward =
							counter.saturating_sub(info.checkpoint).saturating_mul_int(info.stake);
						candidate_rewards.push((candidate.clone(), reward));
						total_rewards.saturating_accrue(reward);
						info.checkpoint = counter;
					});
				}
				user_stake_info.maybe_last_reward_session = Some(current_session);
				if !total_rewards.is_zero() {
					Self::do_reward_single(who, total_rewards)?;
					ClaimableRewards::<T>::mutate(|claimable_rewards| {
						claimable_rewards.saturating_reduce(total_rewards);
					});
					let autocompound = AutoCompoundSettings::<T>::get(Layer::Commit, who);
					if autocompound {
						Self::do_lock(who, total_rewards)?;
						for (candidate, rewards) in &candidate_rewards {
							if !rewards.is_zero() {
								Self::do_stake(who, candidate, *rewards)?;
							}
						}
					}
				}
				Ok(candidate_rewards.len() as u32)
			})
		}

		/// Computes pending rewards for a given user.
		/// This function is intended to be used in the runtime API implementation.
		pub fn calculate_unclaimed_rewards(who: &T::AccountId) -> BalanceOf<T> {
			let mut total_rewards: BalanceOf<T> = Zero::zero();
			let user_stake_info = UserStake::<T>::get(who);
			for candidate in &user_stake_info.candidates {
				let counter = Counters::<T>::get(candidate);
				let info = CandidateStake::<T>::get(candidate, who);
				let reward = counter.saturating_sub(info.checkpoint).saturating_mul_int(info.stake);
				total_rewards.saturating_accrue(reward);
			}
			total_rewards
		}

		/// Registers a given account as candidate.
		///
		/// The account has to lock the candidacy bond. If the account was previously a candidate
		/// the retained stake will be re-included.
		///
		/// Returns the registered candidate info.
		pub fn do_register_as_candidate(
			who: &T::AccountId,
			bond: BalanceOf<T>,
		) -> Result<CandidateInfoOf<T>, DispatchError> {
			let min_bond = MinCandidacyBond::<T>::get();
			ensure!(bond >= min_bond, Error::<T>::InvalidCandidacyBond);

			let available_balance = Self::get_free_balance(who);
			ensure!(available_balance >= bond, Error::<T>::InsufficientFreeBalance);

			// First authored block is current block plus kick threshold to handle session delay
			let candidate = Candidates::<T>::try_mutate_exists(
				who,
				|maybe_candidate_info| -> Result<CandidateInfoOf<T>, DispatchError> {
					ensure!(maybe_candidate_info.is_none(), Error::<T>::AlreadyCandidate);
					LastAuthoredBlock::<T>::insert(
						who.clone(),
						Self::current_block_number().saturating_add(T::KickThreshold::get()),
					);
					// In case we are dealing with an ex-candidate that re-joins, count the current
					// stake and stakers.
					let mut stake: BalanceOf<T> = Zero::zero();
					let mut stakers: u32 = Zero::zero();
					for (_, info) in CandidateStake::<T>::iter_prefix(who) {
						stake.saturating_accrue(info.stake);
						stakers.saturating_inc();
					}
					// Users are allowed to fully claim the old candidacy bond as long as they were
					// replaced by another candidate.
					CandidacyBondReleases::<T>::try_mutate(
						who,
						|maybe_bond_release| -> DispatchResult {
							if let Some(bond_release) = maybe_bond_release {
								if bond_release.reason == CandidacyBondReleaseReason::Replaced {
									Self::decrease_frozen(who, bond_release.bond)?;
									LockedBalances::<T>::mutate(who, |lock| {
										lock.releasing.saturating_reduce(bond_release.bond)
									});
									*maybe_bond_release = None;
								}
							}
							Ok(())
						},
					)?;
					let info = CandidateInfo { stake, stakers };
					*maybe_candidate_info = Some(info.clone());

					// If the candidate left in the current session and is now rejoining
					// remove it from the SessionRemovedCandidates
					SessionRemovedCandidates::<T>::remove(who);

					LockedBalances::<T>::mutate(who, |lock| lock.candidacy_bond = bond);
					Self::increase_frozen(who, bond)?;
					Ok(info)
				},
			)?;

			Self::deposit_event(Event::CandidateAdded { account: who.clone(), deposit: bond });
			Ok(candidate)
		}

		/// Releases all pending release requests for a given user that are expired.
		///
		/// Returns the amount of operations performed.
		pub fn do_release(who: &T::AccountId) -> Result<u32, DispatchError> {
			let mut released: BalanceOf<T> = 0u32.into();
			let mut pos = 0;
			ReleaseQueues::<T>::mutate_exists(who, |maybe_requests| {
				if let Some(requests) = maybe_requests {
					let curr_block = Self::current_block_number();
					for request in requests.iter() {
						if request.block > curr_block {
							break;
						}
						pos += 1;
						released.saturating_accrue(request.amount);
					}
					requests.drain(..pos);
					return if requests.is_empty() { None } else { Some(()) };
				}
				None
			});
			if !released.is_zero() {
				Self::decrease_frozen(who, released)?;
				LockedBalances::<T>::mutate_exists(who, |maybe_locked| {
					if let Some(locked) = maybe_locked {
						locked.releasing.saturating_reduce(released);
						// Remove the map entry if needed be to save some space.
						if locked.total().is_zero() {
							*maybe_locked = None;
						}
					}
				});
				Self::deposit_event(Event::StakeReleased {
					account: who.clone(),
					amount: released,
				});
			}
			Ok(pos as u32)
		}

		/// Notes the last unstake operation for a given user
		fn note_last_unstake(account: &T::AccountId, amount: BalanceOf<T>) {
			UserStake::<T>::mutate(account, |info| {
				let (balance, block) = info.maybe_last_unstake.unwrap_or_default();
				let now = Self::current_block_number();
				let final_amount =
					if now > block { amount } else { amount.saturating_add(balance) };
				info.maybe_last_unstake =
					Some((final_amount, now.saturating_add(T::RestakeUnlockDelay::get())));
			});
		}

		/// Adds stake into a given candidate by providing its address and the amount to stake.
		///
		/// This operation will fail if:
		///   - The user does not have sufficient locked balance to perform this operation.
		///   - The candidate is not registered as such.
		///   - The total staked amount for this staker in this candidate is lower than [`MinStake`].
		///   - The amount of stakers for this candidate is greater than or equal to [`MaxStakers`]
		///     and the staker did not previously have stake on this candidate.
		///   - The staker staked on more than [`MaxStakedCandidates`] candidates.
		fn do_stake(
			staker: &T::AccountId,
			candidate: &T::AccountId,
			amount: BalanceOf<T>,
		) -> Result<(), DispatchError> {
			let UserStakeInfo { stake: currently_staked, .. } = UserStake::<T>::get(staker);
			let frozen_balance = Self::get_staked_balance(staker);
			ensure!(
				frozen_balance.saturating_sub(currently_staked) >= amount,
				Error::<T>::InsufficientLockedBalance
			);

			let current_session = CurrentSession::<T>::get();
			Candidates::<T>::try_mutate(candidate, |maybe_candidate_info| -> DispatchResult {
				let mut candidate_info =
					maybe_candidate_info.clone().ok_or(Error::<T>::NotCandidate)?;
				CandidateStake::<T>::try_mutate(
					candidate,
					staker,
					|candidate_stake_info| -> DispatchResult {
						let final_staker_stake = candidate_stake_info.stake.saturating_add(amount);
						ensure!(
							final_staker_stake >= MinStake::<T>::get(),
							Error::<T>::InsufficientStake
						);
						let is_first_time = candidate_stake_info.stake.is_zero();
						if is_first_time {
							ensure!(
								candidate_info.stakers < T::MaxStakers::get(),
								Error::<T>::TooManyStakers
							);
							candidate_info.stakers.saturating_inc();
						}
						candidate_stake_info.stake = final_staker_stake;
						candidate_stake_info.checkpoint = Counters::<T>::get(candidate);
						candidate_info.stake.saturating_accrue(amount);
						UserStake::<T>::try_mutate(staker, |user_stake_info| -> DispatchResult {
							// In case the user recently unstaked we cannot allow those funds to be quickly
							// reinvested. Otherwise, stakers could potentially move funds right before
							// the session ends from one candidate to another, depending on the most
							// performant ones during the current session.
							if let Some((unavailable_amount, block_limit)) =
								user_stake_info.maybe_last_unstake
							{
								if block_limit > Self::current_block_number() {
									let available_amount = frozen_balance
										.saturating_sub(unavailable_amount)
										.saturating_sub(user_stake_info.stake);
									ensure!(
										available_amount >= amount,
										Error::<T>::InsufficientLockedBalance
									);
								} else {
									user_stake_info.maybe_last_unstake = None;
								}
							}

							user_stake_info
								.candidates
								.try_insert(candidate.clone())
								.map_err(|_| Error::<T>::TooManyStakedCandidates)?;
							user_stake_info.stake.saturating_accrue(amount);
							if user_stake_info.maybe_last_reward_session.is_none() {
								user_stake_info.maybe_last_reward_session = Some(current_session);
							}
							Ok(())
						})?;

						Self::deposit_event(Event::StakeAdded {
							account: staker.clone(),
							candidate: candidate.clone(),
							amount,
						});
						Ok(())
					},
				)?;
				*maybe_candidate_info = Some(candidate_info);
				Ok(())
			})
		}

		/// Returns the total number of accounts that are eligible collators (both candidates and
		/// invulnerables).
		pub fn eligible_collators() -> u32 {
			Candidates::<T>::count()
				.saturating_add(Invulnerables::<T>::decode_len().unwrap_or_default() as u32)
		}

		/// Checks if the given staker has already claimed their rewards for the current session.
		pub fn staker_has_claimed(who: &T::AccountId) -> bool {
			UserStake::<T>::get(who)
				.maybe_last_reward_session
				// Theoretically you cannot receive rewards in the future, but regardless, it
				// should yield the same result.
				.map(|last_reward_session| last_reward_session >= CurrentSession::<T>::get())
				.unwrap_or(true)
		}

		/// Unstakes all funds deposited by `staker` in a given `candidate`.
		///
		/// Returns the amount unstaked, and whether it was unstaked from a candidate or not.
		fn do_unstake(
			staker: &T::AccountId,
			candidate: &T::AccountId,
		) -> Result<(BalanceOf<T>, bool), DispatchError> {
			let stake = Self::remove_stake(candidate, staker);
			let mut is_candidate = false;

			if !stake.is_zero() {
				Candidates::<T>::mutate_exists(candidate, |maybe_info| {
					if let Some(info) = maybe_info {
						is_candidate = true;
						info.stake.saturating_reduce(stake);
						info.stakers.saturating_dec();
					}
				});
			}

			Ok((stake, is_candidate))
		}

		/// Enables or disables autocompounding for the given staker.
		///
		/// Autocompounding automatically reinvests rewards if the staked balance
		/// is above the required threshold.
		///
		/// - `who`: The account to enable or disable autocompounding for.
		/// - `enable`: Boolean indicating whether to enable or disable autocompounding.
		///
		/// Emits `AutoCompoundEnabled` or `AutoCompoundDisabled` event based on the action.
		fn do_set_autocompound(who: &T::AccountId, enable: bool) -> DispatchResult {
			let already_enabled = AutoCompoundSettings::<T>::get(Layer::Commit, who);
			let is_delivering_rewards = Self::is_delivering_rewards();

			if !enable {
				if is_delivering_rewards {
					AutoCompoundSettings::<T>::insert(Layer::Staging, who, false);
				} else {
					AutoCompoundSettings::<T>::remove(Layer::Staging, who);
					AutoCompoundSettings::<T>::remove(Layer::Commit, who);
				}
				if already_enabled {
					Self::deposit_event(Event::AutoCompoundDisabled { account: who.clone() });
				}
			} else {
				let layer = if is_delivering_rewards { Layer::Staging } else { Layer::Commit };
				ensure!(
					Self::get_staked_balance(who) >= T::AutoCompoundingThreshold::get(),
					Error::<T>::InsufficientStake
				);
				AutoCompoundSettings::<T>::insert(layer, who, true);
				if !already_enabled {
					Self::deposit_event(Event::AutoCompoundEnabled { account: who.clone() });
				}
			}

			// If we could write directly into the commit layer then we can safely remove the staging one.
			if !is_delivering_rewards {
				AutoCompoundSettings::<T>::remove(Layer::Staging, who);
			}

			Ok(())
		}

		/// Disable autocompounding if staked balance dropped below the threshold.
		fn adjust_autocompound(staker: &T::AccountId) {
			if Self::get_staked_balance(staker) < T::AutoCompoundingThreshold::get() {
				// This cannot fail if setting to `false`.
				let _ = Self::do_set_autocompound(staker, false);
			}
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
							match user_stake_info.candidates.len() {
								// We must maintain the last unstake operation.
								0..=1 => {
									if let Some(last_unstake) = user_stake_info.maybe_last_unstake {
										*user_stake_info = UserStakeInfo {
											maybe_last_unstake: Some(last_unstake),
											..Default::default()
										}
									} else {
										*maybe_user_stake_info = None;
									}
								},
								_ => {
									user_stake_info
										.stake
										.saturating_reduce(candidate_stake_info.stake);
									user_stake_info.candidates.remove(candidate);
								},
							}
						}
					});
				}
				*maybe_candidate_stake_info = None;
			});
			Self::deposit_event(Event::StakeRemoved {
				account: staker.clone(),
				candidate: candidate.clone(),
				amount: stake,
			});
			stake
		}

		/// Attempts to remove a candidate, identified by its account.
		///
		/// Returns the candidate info prior to its removal.
		pub fn try_remove_candidate(
			who: &T::AccountId,
			remove_last_authored: bool,
			reason: CandidacyBondReleaseReason,
		) -> Result<CandidateInfoOf<T>, DispatchError> {
			Candidates::<T>::try_mutate_exists(
				who,
				|maybe_candidate| -> Result<CandidateInfoOf<T>, DispatchError> {
					let candidate = maybe_candidate.clone().ok_or(Error::<T>::NotCandidate)?;
					if remove_last_authored {
						LastAuthoredBlock::<T>::remove(who.clone())
					}
					Self::release_candidacy_bond(who, reason)?;

					// Store removed candidate in SessionRemovedCandidates to properly reward
					// the candidate and its stakers at the end of the session.
					SessionRemovedCandidates::<T>::insert(who, candidate.clone());

					Self::deposit_event(Event::CandidateRemoved { account: who.clone() });
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
			let now = Self::current_block_number();
			let block = now.saturating_add(delay);
			ReleaseQueues::<T>::try_mutate(account, |requests| -> DispatchResult {
				requests
					.try_push(ReleaseRequest { block, amount })
					.map_err(|_| Error::<T>::TooManyReleaseRequests)?;
				LockedBalances::<T>::mutate(account, |lock| {
					lock.releasing.saturating_accrue(amount);
				});
				Ok(())
			})?;
			// Since the process of unstaking leads to penalties, this lets users stake new funds
			// without penalties on them, while still tracking their previously unstaked funds.
			// If the user just unstaked and unlocked the funds we can decrease the unavailable amount
			// to stake. This is to allow users that decided to lock more funds during the penalty
			// period not to have a penalty for funds that are no longer available to be staked, since
			// they are using "new" funds.
			UserStake::<T>::mutate(account, |user_stake_info| {
				if let Some((unavailable_amount, block_limit)) = user_stake_info.maybe_last_unstake
				{
					let new_unavailable_amount = unavailable_amount.saturating_sub(amount);
					user_stake_info.maybe_last_unstake =
						if new_unavailable_amount.is_zero() || now > block_limit {
							None
						} else {
							Some((new_unavailable_amount, block_limit))
						}
				}
			});
			Self::deposit_event(Event::ReleaseRequestCreated {
				account: account.clone(),
				amount,
				block,
			});
			Ok(())
		}

		/// Decreases the frozen balance for staking.
		pub(crate) fn decrease_frozen(
			who: &T::AccountId,
			amount: BalanceOf<T>,
		) -> Result<(), DispatchError> {
			T::Currency::decrease_frozen(&FreezeReason::Staking.into(), who, amount)
		}

		/// Increases the frozen balance for staking.
		pub(crate) fn increase_frozen(
			who: &T::AccountId,
			amount: BalanceOf<T>,
		) -> Result<(), DispatchError> {
			T::Currency::increase_frozen(&FreezeReason::Staking.into(), who, amount)
		}

		/// Prepares the candidacy bond to be released.
		fn release_candidacy_bond(
			account: &T::AccountId,
			reason: CandidacyBondReleaseReason,
		) -> DispatchResult {
			// First attempt to claim a hypothetical older candidacy bond in case the user forgot
			// to do so before leaving the candidacy list.
			Self::do_claim_candidacy_bond(account)?;

			let bond = Self::get_bond(account);
			if !bond.is_zero() {
				// And finally update the period.
				let release_block =
					Self::current_block_number().saturating_add(T::BondUnlockDelay::get());
				CandidacyBondReleases::<T>::mutate(account, |maybe_bond_release| {
					let mut final_bond = bond;
					if let Some(CandidacyBondRelease { bond: previous_bond, .. }) =
						maybe_bond_release
					{
						// In case there exists a previous bond that could not be claimed at the
						// beginning of this function, it gets accumulated with this new bond that
						// has just been released.
						final_bond.saturating_accrue(*previous_bond);
					}
					*maybe_bond_release = Some(CandidacyBondRelease {
						bond: final_bond,
						block: release_block,
						reason,
					});
				});
				LockedBalances::<T>::mutate(account, |lock| {
					lock.candidacy_bond = Zero::zero();
					lock.releasing.saturating_accrue(bond);
				});
			}
			Ok(())
		}

		/// Claims the candidacy bond, provided sufficient time has passed.
		fn do_claim_candidacy_bond(account: &T::AccountId) -> DispatchResult {
			CandidacyBondReleases::<T>::try_mutate(account, |maybe_bond_release| {
				if let Some(CandidacyBondRelease { bond, block: bond_release, .. }) =
					maybe_bond_release
				{
					if Self::current_block_number() > *bond_release {
						Self::decrease_frozen(account, *bond)?;
						LockedBalances::<T>::mutate_exists(account, |maybe_locked| {
							if let Some(locked) = maybe_locked {
								locked.releasing.saturating_reduce(*bond);
								if locked.total().is_zero() {
									*maybe_locked = None;
								}
							}
						});
						*maybe_bond_release = None;
					}
				}
				// We always return a success, as it is not an error if the candidacy bond
				// is not ready to be claimed yet.
				Ok(())
			})
		}

		/// Checks whether rewards are currently being delivered by the system or not.
		pub(crate) fn is_delivering_rewards() -> bool {
			matches!(NextSystemOperation::<T>::get(), Operation::RewardStakers { .. })
		}

		/// Rewards all collators for a given session.
		///
		/// Returns a tuple with the number of rewardable collators and the total rewards for the
		/// current session.
		fn reward_collators(session: SessionIndex) -> (u32, BalanceOf<T>) {
			let claimable_rewards = ClaimableRewards::<T>::get();
			let total_rewards =
				T::Currency::reducible_balance(&Self::account_id(), Preserve, Polite)
					.saturating_sub(claimable_rewards);

			let mut stakers_total_rewards: BalanceOf<T> = Zero::zero();
			let mut rewardable_collators: u32 = Zero::zero();
			let (_, rewardable_blocks) = TotalBlocks::<T>::get();
			let produced_blocks: Vec<_> = ProducedBlocks::<T>::drain().collect();
			if !rewardable_blocks.is_zero() && !total_rewards.is_zero() {
				let collator_percentage = CollatorRewardPercentage::<T>::get();
				for (collator, blocks) in produced_blocks {
					// Get the collator info of a candidate, in the case that the collator was removed from the
					// candidate list during the session, the collator and its stakers must still be rewarded
					// for the produced blocks in the session so the info can be obtained from SessionRemovedCandidates.
					let maybe_collator_info = Self::get_candidate(&collator)
						.or_else(|_| {
							SessionRemovedCandidates::<T>::take(&collator)
								.ok_or(Error::<T>::NotRemovedCandidate)
						})
						.ok();

					if let Some(collator_info) = maybe_collator_info {
						if blocks > rewardable_blocks {
							// The only case this could happen is if the candidate was an invulnerable during the session.
							// Since blocks produced by invulnerables are not currently stored in ProducedBlocks this error
							// should not occur.
							log::warn!("Cannot reward collator {:?} for producing more blocks than rewardable ones", collator);
							break;
						}
						let ratio = Perbill::from_rational(blocks, rewardable_blocks);
						let rewards_all = ratio * total_rewards;
						let collator_only_reward = collator_percentage.mul_floor(rewards_all);
						let stakers_only_rewards = rewards_all.saturating_sub(collator_only_reward);
						// Reward the collator. Note these rewards are not autocompounded.
						if let Err(error) = Self::do_reward_single(&collator, collator_only_reward)
						{
							log::warn!(target: LOG_TARGET, "Failure rewarding collator {:?}: {:?}", collator, error);
						}

						// No rewards for stakers if:
						// - The collator has no stakers.
						// - The actual reward is zero.
						// - It is the first session. This is because stakers do not receive rewards
						//   for the first session they stake in, so in the worst case they staked
						//   in session zero.
						if collator_info.stake.is_zero()
							|| session.is_zero() || stakers_only_rewards.is_zero()
						{
							break;
						}
						rewardable_collators.saturating_inc();
						stakers_total_rewards.saturating_accrue(stakers_only_rewards);

						// Increase the reward counter for this collator.
						let session_ratio = FixedU128::saturating_from_rational(
							stakers_only_rewards,
							collator_info.stake,
						);
						Counters::<T>::mutate(&collator, |counter| {
							counter.saturating_accrue(session_ratio)
						});
					} else {
						log::warn!("Collator {:?} is no longer a candidate", collator);
					}
				}

				// Start the process to automatically collect the rewards in on_idle.
				NextSystemOperation::<T>::set(Operation::RewardStakers {
					maybe_last_processed_account: None,
				});
				ClaimableRewards::<T>::set(claimable_rewards.saturating_add(stakers_total_rewards));
			}

			(rewardable_collators, total_rewards)
		}

		/// Locks the provided `amount` from `account` for staking.
		///
		/// The operation will fail if `account` does not have sufficient free balance.
		fn do_lock(account: &T::AccountId, amount: BalanceOf<T>) -> DispatchResult {
			Self::increase_frozen(account, amount)?;
			Self::deposit_event(Event::<T>::LockExtended { account: account.clone(), amount });
			Ok(())
		}

		/// Rewards a single account.
		///
		/// If the reward is zero, this is a no-op.
		fn do_reward_single(who: &T::AccountId, reward: BalanceOf<T>) -> DispatchResult {
			if !reward.is_zero() {
				T::Currency::transfer(&Self::account_id(), who, reward, Preserve)?;
				Self::deposit_event(Event::StakingRewardReceived {
					account: who.clone(),
					amount: reward,
				});
			}
			Ok(())
		}

		/// Gets the current block number.
		pub fn current_block_number() -> BlockNumberFor<T> {
			frame_system::Pallet::<T>::block_number()
		}

		pub fn get_total_frozen_balance(account: &T::AccountId) -> BalanceOf<T> {
			T::Currency::balance_frozen(&FreezeReason::Staking.into(), account)
		}

		/// Gets the locked balance potentially used for staking.
		pub fn get_staked_balance(account: &T::AccountId) -> BalanceOf<T> {
			Self::get_total_frozen_balance(account)
				.saturating_sub(LockedBalances::<T>::get(account).total())
		}

		/// Gets the locked balance to be released.
		pub fn get_releasing_balance(account: &T::AccountId) -> BalanceOf<T> {
			LockedBalances::<T>::get(account).releasing
		}

		/// Gets the locked balance for the candidacy bond.
		pub fn get_bond(account: &T::AccountId) -> BalanceOf<T> {
			LockedBalances::<T>::get(account).candidacy_bond
		}

		/// Gets the maximum balance a given user can lock for staking.
		pub fn get_free_balance(account: &T::AccountId) -> BalanceOf<T> {
			let total_locked = Self::get_total_frozen_balance(account);
			T::Currency::balance(account).saturating_sub(total_locked)
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
		pub fn get_sorted_candidate_list() -> Vec<(T::AccountId, CandidateInfoOf<T>)> {
			let mut all_candidates = Candidates::<T>::iter().collect::<Vec<_>>();
			all_candidates.sort_by(|(_, info1), (_, info2)| info2.stake.cmp(&info1.stake));
			all_candidates
		}

		/// Retrieve the list of candidates sorted by stake.
		///
		/// **Note:** This function is intended for use within the runtime API only.
		pub fn candidates() -> Vec<(T::AccountId, BalanceOf<T>)> {
			Self::get_sorted_candidate_list()
				.into_iter()
				.map(|(acc, info)| (acc, info.stake))
				.collect()
		}

		/// Kicks out candidates that did not produce a block in the kick threshold, and refunds
		/// the stakers. The candidate is refunded after a delay.
		///
		/// Return value is the number of candidates left in the list.
		pub fn kick_stale_candidates() -> u32 {
			let now = Self::current_block_number();
			let kick_threshold = T::KickThreshold::get();
			let min_collators = T::MinEligibleCollators::get();
			let min_candidacy_bond = MinCandidacyBond::<T>::get();
			Candidates::<T>::iter()
                .filter_map(|(who, info)| {
                    let last_block = LastAuthoredBlock::<T>::get(who.clone());
                    let since_last = now.saturating_sub(last_block);
                    let is_lazy = since_last >= kick_threshold;
                    let bond = Self::get_bond(&who);

                    if Self::eligible_collators() <= min_collators || (!is_lazy && bond >= min_candidacy_bond) {
                        // Either this is a good collator (not lazy) or we are at the minimum
                        // that the system needs. They get to stay, as long as they have sufficient deposit plus stake.
                        Some(info)
                    } else {
                        // This collator has not produced a block recently enough. Bye bye.
                        match Self::try_remove_candidate(&who, true, CandidacyBondReleaseReason::Idle) {
							Ok(_) => None,
							Err(error) => {
								log::warn!("Could not remove candidate {:?}: {:?}", info, error);
								Some(info)
							},
						}
                    }
                })
                .count()
                .try_into()
                .expect("filter_map operation can't result in a bounded vec larger than its original; qed")
		}

		/// Returns the candidate with the lowest candidacy bond.
		fn get_worst_candidate() -> Result<(T::AccountId, BalanceOf<T>), DispatchError> {
			let mut all_candidates = Candidates::<T>::iter()
				.map(|(candidate, _)| (candidate.clone(), Self::get_bond(&candidate)))
				.collect::<Vec<_>>();
			all_candidates.sort_by(|(_, bond1), (_, bond2)| bond2.cmp(bond1));
			let candidate = all_candidates.last().ok_or(Error::<T>::TooManyCandidates)?;
			Ok(candidate.clone())
		}

		/// Removes the candidate with the lowest bond, as long as it is lower than `bond`.
		pub(crate) fn remove_worst_candidate(
			bond: BalanceOf<T>,
		) -> Result<T::AccountId, DispatchError> {
			let (candidate, worst_bond) = Self::get_worst_candidate()?;
			ensure!(bond > worst_bond, Error::<T>::InvalidCandidacyBond);
			Self::try_remove_candidate(&candidate, false, CandidacyBondReleaseReason::Replaced)?;
			Ok(candidate)
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
		/// ## [`MaxStakedCandidates`]
		///
		/// * The amount of staked candidates per account is limited and its maximum value must not be surpassed.
		///
		/// ## [`Candidates`]
		///
		/// * The amount of stakers per Candidate is limited and its maximum value must not be surpassed.
		/// * The number of candidates should not exceed the candidate list capacity
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
					.all(|UserStakeInfo { candidates, .. }| (candidates.len() as u32)
						<= T::MaxStakedCandidates::get()),
				"Stake count must not exceed MaxStakedCandidates"
			);

			ensure!(
				Candidates::<T>::iter_values()
					.all(|CandidateInfo { stakers, .. }| stakers <= T::MaxStakers::get()),
				"Staker count must not exceed MaxStakers"
			);

			ensure!(
				Candidates::<T>::count() <= T::MaxCandidates::get(),
				"Candidate count must not exceed MaxCandidates"
			);

			for (account, lock) in LockedBalances::<T>::iter() {
				let frozen_balance = Self::get_total_frozen_balance(&account);
				ensure!(
					lock.total() <= frozen_balance,
					"Staker has a mismatch between locked funds and tracked ones"
				);
				ensure!(
					!frozen_balance.is_zero(),
					"LockedBalances holds an item with zero actual locked balances. It should be removed"
				);
				let new_releases = ReleaseQueues::<T>::get(&account);
				let normal_release: BalanceOf<T> = new_releases
					.iter()
					.map(|r| r.amount)
					.reduce(|a, b| a.saturating_add(b))
					.unwrap_or_default();
				let bond_release =
					CandidacyBondReleases::<T>::get(&account).map(|r| r.bond).unwrap_or_default();
				ensure!(
					(bond_release + normal_release) == Self::get_releasing_balance(&account),
					"Releasing balance is not the sum of the candidacy bond and the release queue"
				);
				#[allow(deprecated)]
				{
					ensure!(
						T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &account)
							.is_zero(),
						"Old candidacy bond lock is present"
					);
					ensure!(
						T::Currency::balance_frozen(&FreezeReason::Releasing.into(), &account)
							.is_zero(),
						"Old releasing lock is present"
					);
				}
			}

			Ok(())
		}
	}

	/// Keep track of number of authored blocks per authority. Uncles are counted as well since
	/// they're a valid proof of being online.
	///
	/// If the account is a candidate, it will get rewards for producing blocks.
	impl<T: Config + pallet_authorship::Config>
		pallet_authorship::EventHandler<T::AccountId, BlockNumberFor<T>> for Pallet<T>
	{
		fn note_author(author: T::AccountId) {
			LastAuthoredBlock::<T>::insert(author.clone(), Self::current_block_number());

			// Invulnerables do not get rewards
			if Self::is_invulnerable(&author) {
				TotalBlocks::<T>::mutate(|(total, _)| {
					total.saturating_inc();
				});
			} else {
				ProducedBlocks::<T>::mutate(author, |b| b.saturating_inc());
				TotalBlocks::<T>::mutate(|(total, rewardable)| {
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

	/// Implementation of the session manager.
	impl<T: Config> SessionManager<T::AccountId> for Pallet<T> {
		fn new_session(_: SessionIndex) -> Option<Vec<T::AccountId>> {
			let candidates_len_before = Candidates::<T>::count();
			let active_candidates_count = Self::kick_stale_candidates();
			let removed = candidates_len_before.saturating_sub(active_candidates_count);
			let result = Self::assemble_collators();

			// Although the removed candidates are passively deleted from SessionRemovedCandidates
			// during the distribution of session rewards, it is possible that a removed candidate
			// is not removed if the candidate didn't produce and blocks during the session. For that
			// reason the leftover keys in the SessionRemovedCandidates StorageMap must be cleared.
			let _ = SessionRemovedCandidates::<T>::clear(T::MaxCandidates::get(), None);

			frame_system::Pallet::<T>::register_extra_weight_unchecked(
				T::WeightInfo::new_session(removed, candidates_len_before),
				DispatchClass::Mandatory,
			);
			Some(result)
		}

		fn start_session(index: SessionIndex) {
			// Initialize counters for this session
			TotalBlocks::<T>::set((0, 0));
			CurrentSession::<T>::set(index);

			frame_system::Pallet::<T>::register_extra_weight_unchecked(
				T::WeightInfo::start_session(),
				DispatchClass::Mandatory,
			);
		}

		fn end_session(index: SessionIndex) {
			// Transfer the extra reward, if any, to the pot.
			let pot_account = Self::account_id();
			let per_block_extra_reward = ExtraReward::<T>::get();
			if !per_block_extra_reward.is_zero() {
				let (produced_blocks, _) = TotalBlocks::<T>::get();
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

			let (total_collators, total_rewards) = Self::reward_collators(index);
			Self::deposit_event(Event::<T>::SessionEnded { index, rewards: total_rewards });

			frame_system::Pallet::<T>::register_extra_weight_unchecked(
				T::WeightInfo::end_session(total_collators),
				DispatchClass::Mandatory,
			);
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
