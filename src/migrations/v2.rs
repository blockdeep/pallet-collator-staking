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

//! Collator Staking Pallet migration from v1 to v2.

#![allow(deprecated)]

use crate::migrations::PALLET_MIGRATIONS_ID;
use crate::{
	AutoCompoundSettings, BalanceOf, CandidacyBondReleases, CandidateStake, CandidateStakeInfo,
	Candidates, ClaimableRewards, Config, FreezeReason, Layer, LockedBalances, Pallet,
	ReleaseQueues, WeightInfo,
};
use core::fmt::Debug;
use frame_support::migrations::{MigrationId, SteppedMigration, SteppedMigrationError};
use frame_support::pallet_prelude::*;
use frame_support::traits::fungible::{InspectFreeze, MutateFreeze};
use frame_support::weights::WeightMeter;
use sp_runtime::{FixedU128, Percent, Saturating};
use sp_std::vec::Vec;

#[cfg(feature = "try-runtime")]
use sp_runtime::TryRuntimeError;
#[cfg(feature = "try-runtime")]
use sp_std::collections::btree_map::BTreeMap;

pub(crate) mod v1 {
	use super::*;
	use frame_support::{storage_alias, Blake2_128Concat};
	use sp_staking::SessionIndex;

	/// Stores information about a stake held by a staker in the checkpoint system of a candidate.
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
		/// The last session where the stake was updated.
		pub session: SessionIndex,
		/// The amount of balance staked by the staker.
		pub stake: Balance,
	}

	/// Storage double map that tracks staking information for candidates and their stakers.
	/// - First Key: The candidate's account ID
	/// - Second Key: The staker's account ID
	/// - Value: Information about the stake amount and its current checkpoint
	#[storage_alias]
	pub type CandidateStake<T: Config> = StorageDoubleMap<
		Pallet<T>,
		Blake2_128Concat,
		<T as frame_system::Config>::AccountId,
		Blake2_128Concat,
		<T as frame_system::Config>::AccountId,
		CandidateStakeInfo<BalanceOf<T>>,
		ValueQuery,
	>;

	/// Storage map that tracks the auto-compound preferences from before the migration.
	#[storage_alias]
	pub type AutoCompound<T: Config> = StorageMap<
		Pallet<T>,
		Blake2_128Concat,
		<T as frame_system::Config>::AccountId,
		Percent,
		ValueQuery,
	>;
}

/// Operations to be performed during this migration.
#[derive(
	PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, scale_info::TypeInfo, MaxEncodedLen,
)]
pub enum MigrationSteps<T: Config> {
	/// The stake has to be migrated from the old storage layout.
	MigrateStake { cursor: Option<(T::AccountId, T::AccountId)> },
	/// Migrate autocompounding.
	MigrateAutocompounding { cursor: Option<T::AccountId> },
	/// Migrate the release queue.
	MigrateReleaseQueue { cursor: Option<T::AccountId> },
	/// Migrate the candidacy bond locked balance.
	MigrateCandidacyBond { cursor: Option<T::AccountId> },
	/// Migrate the candidacy bond pending releases.
	MigrateCandidacyBondReleases { cursor: Option<T::AccountId> },
	/// [`ClaimableRewards`] are to be set to zero, resetting all rewards.
	ResetClaimableRewards,
	/// Changes the storage version to 2.
	ChangeStorageVersion,
	/// No more operations to be performed.
	Noop,
}

/// Migrates the items of the [`CandidateStake`] map to the counter-checkpoint
/// reward-tracking system.
///
/// The `step` function will be called once per block. It is very important that this function
/// *never* panics and never uses more weight than it got in its meter. The migrations should also
/// try to make maximal progress per step so that the total time it takes to migrate stays low.
pub struct LazyMigrationV1ToV2<T: Config + Debug>(PhantomData<T>);

impl<T: Config + Debug> LazyMigrationV1ToV2<T> {
	pub(crate) fn do_migrate_autocompounding(
		meter: &mut WeightMeter,
		cursor: &mut Option<T::AccountId>,
	) {
		let required =
			<T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_autocompound_step();

		let mut iter = if let Some(staker) = cursor.clone() {
			v1::AutoCompound::<T>::iter_from_key(staker)
		} else {
			v1::AutoCompound::<T>::iter()
		};

		while meter.try_consume(required).is_ok() {
			if let Some((staker, value)) = iter.next() {
				v1::AutoCompound::<T>::remove(staker.clone());
				if !value.is_zero() {
					AutoCompoundSettings::<T>::insert(Layer::Commit, staker.clone(), true);
				}
				*cursor = Some(staker);
			} else {
				*cursor = None;
				break;
			}
		}
	}

	pub(crate) fn do_migrate_release_queue(
		meter: &mut WeightMeter,
		cursor: &mut Option<T::AccountId>,
	) {
		let required = <T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_release_queue(
			<T as Config>::MaxStakedCandidates::get(),
		);

		let now = Pallet::<T>::current_block_number();
		let mut iter = match cursor {
			None => ReleaseQueues::<T>::iter(),
			Some(key) => ReleaseQueues::<T>::iter_from_key(key),
		};
		while meter.can_consume(required) {
			if let Some((staker, requests)) = iter.next() {
				meter.consume(
					<T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_release_queue(
						requests.len() as u32,
					),
				);
				let mut total_released: BalanceOf<T> = Zero::zero();
				let _ = T::Currency::thaw(&FreezeReason::Releasing.into(), &staker);
				let remaining_requests = requests
					.into_iter()
					.filter(|release| {
						// If the release has already expired, we can simply remove it.
						if now > release.block {
							return false;
						}
						// Attempt to increase the frozen balance of the staker if the release is
						// still active.
						// If it fails, it means the user was able to spend these funds already, so we
						// can simply remove the release request now.
						match Pallet::<T>::increase_frozen(&staker, release.amount) {
							Ok(_) => {
								total_released.saturating_accrue(release.amount);
								true
							},
							Err(e) => {
								log::warn!(
									"Failed to increase frozen balance of {:?} when releasing: {:?}",
									staker,
									e
								);
								false
							},
						}
					})
					.collect::<Vec<_>>();
				if !total_released.is_zero() {
					LockedBalances::<T>::mutate(&staker, |locked| {
						locked.releasing.saturating_accrue(total_released);
					});
				}
				if remaining_requests.is_empty() {
					ReleaseQueues::<T>::remove(staker.clone());
				} else {
					ReleaseQueues::<T>::set(
						staker.clone(),
						remaining_requests
							.try_into()
							.expect("Filtering an array must yield a smaller one. qed"),
					);
				}
				*cursor = Some(staker);
			} else {
				meter.consume(
					<T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_release_queue(1),
				);
				*cursor = None;
				break;
			}
		}
	}

	pub(crate) fn do_migrate_candidacy_bond(
		meter: &mut WeightMeter,
		cursor: &mut Option<T::AccountId>,
	) {
		let required = <T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_candidacy_bond();
		let mut iter = match cursor {
			None => Candidates::<T>::iter(),
			Some(key) => Candidates::<T>::iter_from(Candidates::<T>::hashed_key_for(key)),
		};

		while meter.try_consume(required).is_ok() {
			if let Some((candidate, _)) = iter.next() {
				let bond =
					T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &candidate);
				let _ = T::Currency::thaw(&FreezeReason::CandidacyBond.into(), &candidate);
				let _ = T::Currency::thaw(&FreezeReason::Releasing.into(), &candidate);
				// Here we attempt to increase the frozen balance of the candidate.
				// If the candidate does not have enough balance to lock,
				// we leave him with a candidacy bond equal to zero.
				match Pallet::<T>::increase_frozen(&candidate, bond) {
					Ok(_) => LockedBalances::<T>::mutate(&candidate, |locked| {
						locked.candidacy_bond.saturating_accrue(bond);
					}),
					Err(e) => {
						log::warn!(
							"Failed to increase frozen balance of {:?} when adjusting the candidacy bond: {:?}",
							candidate,
							e
						);
					},
				}
				*cursor = Some(candidate);
			} else {
				*cursor = None;
				break;
			}
		}
	}

	pub(crate) fn do_migrate_candidacy_bond_releases(
		meter: &mut WeightMeter,
		cursor: &mut Option<T::AccountId>,
	) {
		let required =
			<T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_candidacy_bond_release();
		let mut iter = match cursor {
			None => CandidacyBondReleases::<T>::iter(),
			Some(key) => CandidacyBondReleases::<T>::iter_from_key(key),
		};
		let now = Pallet::<T>::current_block_number();

		while meter.try_consume(required).is_ok() {
			if let Some((excandidate, bond_release)) = iter.next() {
				let _ = T::Currency::thaw(&FreezeReason::Releasing.into(), &excandidate);
				if now >= bond_release.block {
					// Collect the bond.
					CandidacyBondReleases::<T>::remove(excandidate.clone());
				} else {
					match Pallet::<T>::increase_frozen(&excandidate, bond_release.bond) {
						Ok(_) => LockedBalances::<T>::mutate(&excandidate, |locked| {
							locked.releasing.saturating_accrue(bond_release.bond);
						}),
						Err(e) => {
							log::warn!(
							"Failed to increase frozen balance of {:?} when adjusting the candidacy bond release: {:?}",
							excandidate,
							e
						);
							// We tried to freeze the bond, but the user already spent the money, so
							// all we can do is just to remove the bond.
							CandidacyBondReleases::<T>::remove(excandidate.clone());
						},
					}
				}

				*cursor = Some(excandidate);
			} else {
				*cursor = None;
				break;
			}
		}
	}

	pub(crate) fn do_migrate_stake(
		meter: &mut WeightMeter,
		cursor: &mut Option<(T::AccountId, T::AccountId)>,
	) {
		// A single operation reads and removes one element from the old map and inserts it in the new one.
		let required = <T as Config>::WeightInfo::migration_from_v1_to_v2_migrate_stake_step();

		let mut iter = if let Some((candidate, staker)) = cursor.clone() {
			// If a cursor is provided, start iterating from the stored value
			// corresponding to the last key processed in the previous step.
			// Note that this only works if the old and the new maps use the same way to hash
			// storage keys.
			v1::CandidateStake::<T>::iter_from(v1::CandidateStake::<T>::hashed_key_for(
				candidate, staker,
			))
		} else {
			// If no cursor is provided, start iterating from the beginning.
			v1::CandidateStake::<T>::iter()
		};

		// We loop here to do as much progress as possible per step.
		while meter.try_consume(required).is_ok() {
			// If there's a next item in the iterator, perform the migration.
			if let Some((candidate, staker, value)) = iter.next() {
				// We can just insert here since the old and the new maps share the same key-space.
				// Otherwise, it would have to invert the concat hash function and re-hash it.
				CandidateStake::<T>::insert(
					candidate.clone(),
					staker.clone(),
					CandidateStakeInfo { stake: value.stake, checkpoint: FixedU128::zero() },
				);
				*cursor = Some((candidate, staker)) // Return the processed key as the new cursor.
			} else {
				*cursor = None; // No more items to process.
				break;
			}
		}
	}

	fn migrate_autocompounding(
		meter: &mut WeightMeter,
		mut cursor: Option<T::AccountId>,
	) -> MigrationSteps<T> {
		Self::do_migrate_autocompounding(meter, &mut cursor);
		match cursor {
			None => Self::migrate_release_queue(meter, None),
			Some(checkpoint) => MigrationSteps::MigrateAutocompounding { cursor: Some(checkpoint) },
		}
	}

	fn migrate_release_queue(
		meter: &mut WeightMeter,
		mut cursor: Option<T::AccountId>,
	) -> MigrationSteps<T> {
		Self::do_migrate_release_queue(meter, &mut cursor);
		match cursor {
			None => Self::migrate_candidacy_bond(meter, None),
			Some(checkpoint) => MigrationSteps::MigrateReleaseQueue { cursor: Some(checkpoint) },
		}
	}

	fn migrate_candidacy_bond(
		meter: &mut WeightMeter,
		mut cursor: Option<T::AccountId>,
	) -> MigrationSteps<T> {
		Self::do_migrate_candidacy_bond(meter, &mut cursor);
		match cursor {
			None => Self::migrate_candidacy_bond_releases(meter, None),
			Some(checkpoint) => MigrationSteps::MigrateCandidacyBond { cursor: Some(checkpoint) },
		}
	}

	fn migrate_candidacy_bond_releases(
		meter: &mut WeightMeter,
		mut cursor: Option<T::AccountId>,
	) -> MigrationSteps<T> {
		Self::do_migrate_candidacy_bond_releases(meter, &mut cursor);
		match cursor {
			None => Self::reset_rewards(meter),
			Some(checkpoint) => {
				MigrationSteps::MigrateCandidacyBondReleases { cursor: Some(checkpoint) }
			},
		}
	}

	fn migrate_stake(
		meter: &mut WeightMeter,
		mut cursor: Option<(T::AccountId, T::AccountId)>,
	) -> MigrationSteps<T> {
		Self::do_migrate_stake(meter, &mut cursor);
		match cursor {
			None => Self::migrate_autocompounding(meter, None),
			Some(checkpoint) => MigrationSteps::MigrateStake { cursor: Some(checkpoint) },
		}
	}

	fn set_storage_version(meter: &mut WeightMeter) -> MigrationSteps<T> {
		let required = T::DbWeight::get().reads_writes(0, 1);
		if meter.try_consume(required).is_ok() {
			StorageVersion::new(Self::id().version_to as u16).put::<Pallet<T>>();
			MigrationSteps::Noop
		} else {
			MigrationSteps::ChangeStorageVersion
		}
	}

	fn reset_rewards(meter: &mut WeightMeter) -> MigrationSteps<T> {
		// This step can be manually calculated.
		let required = T::DbWeight::get().reads_writes(0, 1);
		if meter.try_consume(required).is_ok() {
			ClaimableRewards::<T>::set(Zero::zero());
			Self::set_storage_version(meter)
		} else {
			MigrationSteps::ResetClaimableRewards
		}
	}
}

impl<T: Config + Debug> SteppedMigration for LazyMigrationV1ToV2<T> {
	type Cursor = MigrationSteps<T>;
	// Without the explicit length here, the construction of the ID would not be infallible.
	type Identifier = MigrationId<23>;

	/// The identifier of this migration. Which should be globally unique.
	fn id() -> Self::Identifier {
		MigrationId { pallet_id: *PALLET_MIGRATIONS_ID, version_from: 1, version_to: 2 }
	}

	/// The actual logic of the migration.
	///
	/// This function is called repeatedly until it returns `Ok(None)`, indicating that the
	/// migration is complete. Ideally, the migration should be designed in such a way that each
	/// step consumes as much weight as possible. However, this is simplified to perform one stored
	/// value mutation per block.
	fn step(
		maybe_cursor: Option<Self::Cursor>,
		meter: &mut WeightMeter,
	) -> Result<Option<Self::Cursor>, SteppedMigrationError> {
		if Pallet::<T>::on_chain_storage_version() != Self::id().version_from as u16 {
			return Ok(None);
		}

		let cursor = maybe_cursor.unwrap_or(MigrationSteps::MigrateStake { cursor: None });
		log::info!("Running migration at step: {:?}", cursor);

		let new_cursor = match cursor {
			MigrationSteps::MigrateStake { cursor: checkpoint } => {
				Self::migrate_stake(meter, checkpoint)
			},
			MigrationSteps::MigrateAutocompounding { cursor: checkpoint } => {
				Self::migrate_autocompounding(meter, checkpoint)
			},
			MigrationSteps::MigrateReleaseQueue { cursor: checkpoint } => {
				Self::migrate_release_queue(meter, checkpoint)
			},
			MigrationSteps::MigrateCandidacyBond { cursor: checkpoint } => {
				Self::migrate_candidacy_bond(meter, checkpoint)
			},
			MigrationSteps::MigrateCandidacyBondReleases { cursor: checkpoint } => {
				Self::migrate_candidacy_bond_releases(meter, checkpoint)
			},
			MigrationSteps::ResetClaimableRewards => Self::reset_rewards(meter),
			MigrationSteps::ChangeStorageVersion => Self::set_storage_version(meter),
			MigrationSteps::Noop => MigrationSteps::Noop,
		};

		match new_cursor {
			MigrationSteps::Noop => {
				log::info!("Migration fully complete");
				Ok(None)
			},
			_ => {
				log::info!("Migration not completed yet: {:?}", new_cursor);
				Ok(Some(new_cursor))
			},
		}
	}

	#[cfg(feature = "try-runtime")]
	fn pre_upgrade() -> Result<Vec<u8>, TryRuntimeError> {
		use codec::Encode;

		// Return the state of the storage before the migration.
		let candidate_stakes: BTreeMap<_, _> =
			v1::CandidateStake::<T>::iter().map(|(k1, k2, v)| ((k1, k2), v)).collect();
		let autocompound = v1::AutoCompound::<T>::iter().collect::<Vec<_>>();
		let releases = ReleaseQueues::<T>::iter().collect::<BTreeMap<_, _>>();
		let bond_releases = CandidacyBondReleases::<T>::iter().collect::<BTreeMap<_, _>>();
		let bonds = Candidates::<T>::iter_keys()
			.map(|candidate| {
				let bond =
					T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &candidate);
				(candidate, bond)
			})
			.collect::<Vec<_>>();
		Ok((candidate_stakes, autocompound, releases, bonds, bond_releases).encode())
	}

	#[cfg(feature = "try-runtime")]
	fn post_upgrade(prev: Vec<u8>) -> Result<(), TryRuntimeError> {
		use codec::Decode;
		// Check the state of the storage after the migration.
		ensure!(
			Pallet::<T>::on_chain_storage_version()
				== StorageVersion::new(Self::id().version_to as u16),
			"Migration post-upgrade failed: the storage version is not the expected one"
		);
		let (
			prev_candidate_stakes,
			prev_autocompound,
			prev_releases,
			prev_bonds,
			prev_bond_releases,
		) = <(
			BTreeMap<(T::AccountId, T::AccountId), v1::CandidateStakeInfo<BalanceOf<T>>>,
			Vec<(T::AccountId, Percent)>,
			BTreeMap<T::AccountId, BoundedVec<crate::ReleaseRequestOf<T>, T::MaxStakedCandidates>>,
			Vec<(T::AccountId, BalanceOf<T>)>,
			BTreeMap<T::AccountId, crate::CandidacyBondReleaseOf<T>>,
		)>::decode(&mut &prev[..])
		.expect("Failed to decode the previous storage state");

		// Check the len of prev and post are the same.
		ensure!(prev_candidate_stakes.len() == CandidateStake::<T>::iter().count(), "Migration failed: the number of items in the CandidateStake storage after the migration is not the same as before");

		for ((candidate, staker), value) in prev_candidate_stakes {
			let new_value = CandidateStake::<T>::get(candidate, staker);
			ensure!(
				value.stake == new_value.stake,
				"Migration failed: the stake after the migration is not the same as before"
			);
			ensure!(
				new_value.checkpoint.is_zero(),
				"Migration failed: the checkpoint after the migration is not zero"
			);
		}

		for (staker, old_releases) in prev_releases.into_iter() {
			ensure!(
				<T as Config>::Currency::balance_frozen(&FreezeReason::Releasing.into(), &staker)
					.is_zero(),
				"Migration failed: the release balance after the migration is not zero"
			);
			let new_releases = ReleaseQueues::<T>::get(&staker);
			ensure!(old_releases.len() >= new_releases.len(), "Migration failed: the number of items in the ReleaseQueue storage after the migration is not less than before");
			let total_releasing_new: BalanceOf<T> = new_releases
				.iter()
				.map(|r| r.amount)
				.reduce(|a, b| a.saturating_add(b))
				.unwrap_or_default();
			let total_releasing_old: BalanceOf<T> = old_releases
				.iter()
				.map(|r| r.amount)
				.reduce(|a, b| a.saturating_add(b))
				.unwrap_or_default();
			ensure!(total_releasing_old >= total_releasing_new, "Migration failed: the total releasing balance after the migration is not greater than before");
			ensure!(
				LockedBalances::<T>::get(&staker).releasing >= total_releasing_new,
				"Migration failed: the total releasing balance after the migration is not correct"
			);
		}

		for (candidate, prev_bond) in prev_bonds.into_iter() {
			ensure!(
				<T as Config>::Currency::balance_frozen(
					&FreezeReason::CandidacyBond.into(),
					&candidate
				)
				.is_zero(),
				"Migration failed: the candidacy bond balance after the migration is not zero"
			);

			let bond = LockedBalances::<T>::get(&candidate).candidacy_bond;
			ensure!(bond == prev_bond || bond.is_zero(),
				"Migration failed: the candidacy bond balance after the migration is not the same as before");
		}

		let bond_releases = CandidacyBondReleases::<T>::iter().collect::<BTreeMap<_, _>>();
		ensure!(
			prev_bond_releases.len() >= bond_releases.len(),
			"Migration failed: the number of items in the CandidacyBondReleases storage after the migration is not the same as before"
		);
		for (excandidate, prev_bond_release) in prev_bond_releases.into_iter() {
			ensure!(
				<T as Config>::Currency::balance_frozen(
					&FreezeReason::Releasing.into(),
					&excandidate
				)
				.is_zero(),
				"Migration failed: the candidacy bond balance after the migration is not zero"
			);
			if let Some(bond_release) = bond_releases.get(&excandidate) {
				ensure!(
					prev_bond_release == *bond_release,
					"Migration failed: the candidacy bond release after the migration is not the same as before"
				);
			}
		}

		let releases = LockedBalances::<T>::iter();
		for (staker, locked) in releases {
			let bond_release = if let Some(release) = CandidacyBondReleases::<T>::get(&staker) {
				release.bond
			} else {
				Zero::zero()
			};
			let stake_releases = ReleaseQueues::<T>::get(&staker)
				.into_iter()
				.map(|r| r.amount)
				.reduce(|a, b| a + b)
				.unwrap_or_default();
			let total = bond_release + stake_releases;
			ensure!(
				locked.releasing == total,
				"Migration failed: the total releasing balance after the migration is not correct"
			);
		}

		for (staker, percentage) in prev_autocompound {
			let value = !percentage.is_zero();
			ensure!(
				AutoCompoundSettings::<T>::get(Layer::Commit, &staker) == value,
				"Migration failed: the autocompound setting after the migration is not the same as before"
			);
		}

		ensure!(
			ClaimableRewards::<T>::get().is_zero(),
			"Migration failed: the claimable rewards after the migration is not zero"
		);

		Ok(())
	}
}

#[cfg(all(test, not(feature = "runtime-benchmarks")))]
mod tests {
	use super::*;
	use crate::mock::*;
	use crate::{
		CandidacyBondRelease, CandidacyBondReleaseReason, CandidacyBondReleases, CandidateInfo,
		MinCandidacyBond, ReleaseRequest,
	};
	use frame_support::assert_ok;
	use frame_support::migrations::MultiStepMigrator;
	use frame_support::traits::{
		fungible::{Inspect, Mutate},
		OnRuntimeUpgrade,
	};

	#[test]
	fn migration_of_single_element_should_work() {
		new_test_ext().execute_with(|| {
			let len = 16;
			StorageVersion::new(1).put::<Pallet<Test>>();
			assert_eq!(Pallet::<Test>::on_chain_storage_version(), 1);
			ClaimableRewards::<Test>::set(100);
			Candidates::<Test>::insert(1, CandidateInfo { stake: 0, stakers: 0 });
			let bond = MinCandidacyBond::<Test>::get();
			assert_ok!(<Test as Config>::Currency::set_freeze(
				&FreezeReason::CandidacyBond.into(),
				&1,
				bond
			));

			v1::CandidateStake::<Test>::insert(
				&1,
				&1,
				v1::CandidateStakeInfo { session: 10, stake: 50 },
			);
			v1::AutoCompound::<Test>::insert(&1, Percent::from_percent(100));
			let mut requests = vec![];
			for _ in 0..len {
				requests.push(ReleaseRequest {
					block: 1000,
					amount: <Test as Config>::Currency::minimum_balance(),
				});
			}
			ReleaseQueues::<Test>::set(1, requests.try_into().unwrap());
			let total_release_balance = len * <Test as Config>::Currency::minimum_balance() + bond;
			assert_ok!(<Test as Config>::Currency::set_freeze(
				&FreezeReason::Releasing.into(),
				&1,
				bond
			));
			CandidacyBondReleases::<Test>::insert(
				&1,
				CandidacyBondRelease {
					bond: 10,
					block: u64::MAX,
					reason: CandidacyBondReleaseReason::Idle,
				},
			);

			// Trigger the runtime upgrade
			assert_eq!(ClaimableRewards::<Test>::get(), 100);
			AllPalletsWithSystem::on_runtime_upgrade();
			initialize_to_block(2);

			assert_eq!(
				CandidateStake::<Test>::get(&1, &1),
				CandidateStakeInfo { stake: 50, checkpoint: FixedU128::zero() }
			);
			assert_eq!(AutoCompoundSettings::<Test>::get(Layer::Commit, &1), true);
			assert_eq!(ClaimableRewards::<Test>::get(), 0);
			assert_eq!(Pallet::<Test>::on_chain_storage_version(), 2);
			assert_eq!(ReleaseQueues::<Test>::get(&1).len(), 16);
			assert_eq!(LockedBalances::<Test>::get(&1).releasing, total_release_balance);

			let old_release_lock =
				<Test as Config>::Currency::balance_frozen(&FreezeReason::Releasing.into(), &1);
			assert_eq!(old_release_lock, 0);
			assert_eq!(LockedBalances::<Test>::get(&1).candidacy_bond, bond);

			let old_candidacy_bond_lock =
				<Test as Config>::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &1);
			assert_eq!(old_candidacy_bond_lock, 0);
			assert_eq!(LockedBalances::<Test>::get(&1).candidacy_bond, bond);
		});
	}

	#[test]
	fn migration_of_many_elements_should_work() {
		new_test_ext().execute_with(|| {
			let len = 16;
			let bond = 10;
			let users = 1_000;
			StorageVersion::new(1).put::<Pallet<Test>>();
			assert_eq!(Pallet::<Test>::on_chain_storage_version(), 1);
			ClaimableRewards::<Test>::set(100_000);

			for i in 1..=users {
				assert_ok!(Balances::mint_into(&i, 100_000));
				v1::CandidateStake::<Test>::insert(
					&i,
					&i,
					v1::CandidateStakeInfo { session: 10, stake: 50 },
				);
				v1::AutoCompound::<Test>::insert(&i, Percent::from_percent(100));
				let mut requests = vec![];
				for r in 0..len {
					let amount = <Test as Config>::Currency::minimum_balance() + r as u64;
					requests.push(ReleaseRequest { block: u64::MAX, amount });
				}
				ReleaseQueues::<Test>::set(i, requests.try_into().unwrap());
				CandidacyBondReleases::<Test>::insert(
					&i,
					CandidacyBondRelease {
						bond,
						block: u64::MAX,
						reason: CandidacyBondReleaseReason::Idle,
					},
				);
			}
			let total_release_balance =
				bond + len * <Test as Config>::Currency::minimum_balance() + len * (len - 1) / 2;

			// Trigger the runtime upgrade
			let initial_block = System::block_number();
			AllPalletsWithSystem::on_runtime_upgrade();
			while <Migrator as MultiStepMigrator>::ongoing() {
				let block = System::block_number();
				assert!(
					block - initial_block <= 200,
					"Migration should not take more than 200 blocks"
				);

				initialize_to_block(block + 1);
			}

			for i in 1..=users {
				assert_eq!(
					CandidateStake::<Test>::get(&i, &i),
					CandidateStakeInfo { stake: 50, checkpoint: FixedU128::zero() }
				);
				assert_eq!(AutoCompoundSettings::<Test>::get(Layer::Commit, &i), true);
				assert_eq!(ReleaseQueues::<Test>::get(&i).len() as u64, len);
				assert_eq!(LockedBalances::<Test>::get(&i).releasing, total_release_balance);

				let old_release_lock =
					<Test as Config>::Currency::balance_frozen(&FreezeReason::Releasing.into(), &i);
				assert_eq!(old_release_lock, 0);

				let old_candidacy_bond_lock = <Test as Config>::Currency::balance_frozen(
					&FreezeReason::CandidacyBond.into(),
					&i,
				);
				assert_eq!(old_candidacy_bond_lock, 0);
			}
			assert_eq!(ClaimableRewards::<Test>::get(), 0);
			assert_eq!(
				AutoCompoundSettings::<Test>::iter_prefix(Layer::Commit).count() as u64,
				users
			);
			assert_eq!(Pallet::<Test>::on_chain_storage_version(), StorageVersion::new(2));
		});
	}
}
