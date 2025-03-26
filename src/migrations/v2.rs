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

use crate::migrations::PALLET_MIGRATIONS_ID;
use crate::{BalanceOf, CandidateStake, CandidateStakeInfo, ClaimableRewards, Config, Pallet};
use frame_support::migrations::{MigrationId, SteppedMigration, SteppedMigrationError};
use frame_support::pallet_prelude::*;
use frame_support::weights::WeightMeter;
use sp_runtime::FixedU128;

#[cfg(feature = "try-runtime")]
use sp_runtime::TryRuntimeError;
#[cfg(feature = "try-runtime")]
use std::collections::BTreeMap;

mod v1 {
	use super::*;
	use frame_support::{storage_alias, Blake2_128Concat};
	use sp_staking::SessionIndex;

	/// Old `CandidateStakeInfo` struct.
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
		pub session: SessionIndex,
		pub stake: Balance,
	}

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
}

/// Operations to be performed during this migration.
#[derive(
	PartialEq,
	Eq,
	Clone,
	Encode,
	Decode,
	RuntimeDebug,
	scale_info::TypeInfo,
	MaxEncodedLen,
)]
pub enum MigrationSteps<T: Config> {
	/// The stake has to be migrated form the old storage layout.
	MigrateStake { cursor: Option<(T::AccountId, T::AccountId)> },
	/// [`crate::ClaimableRewards`] are to be set to zero, resetting all rewards.
	ResetClaimableRewards,
	/// No more operations to be performed.
	Noop,
}

/// Migrates the items of the [`crate::CandidateStake`] map to the counter-checkpoint
/// reward-tracking system.
///
/// The `step` function will be called once per block. It is very important that this function
/// *never* panics and never uses more weight than it got in its meter. The migrations should also
/// try to make maximal progress per step, so that the total time it takes to migrate stays low.
pub struct LazyMigrationV2<T: Config>(PhantomData<T>);

impl<T: Config> LazyMigrationV2<T> {
	pub(crate) fn reset_rewards(meter: &mut WeightMeter) -> MigrationSteps<T> {
		let required = T::DbWeight::get().reads_writes(0, 1);
		if meter.try_consume(required).is_ok() {
			ClaimableRewards::<T>::set(Zero::zero());
			MigrationSteps::Noop
		} else {
			MigrationSteps::ResetClaimableRewards
		}
	}

	pub(crate) fn migrate_stake(
		meter: &mut WeightMeter,
		mut cursor: Option<(T::AccountId, T::AccountId)>,
	) -> MigrationSteps<T> {
		// A single operation reads and removes one element from the old map and inserts it in the new one.
		let required = T::DbWeight::get().reads_writes(1, 1);

		let mut iter = if let Some((candidate, staker)) = cursor.clone() {
			// If a cursor is provided, start iterating from the stored value
			// corresponding to the last key processed in the previous step.
			// Note that this only works if the old and the new map use the same way to hash
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
				// We can just insert here since the old and the new map share the same key-space.
				// Otherwise, it would have to invert the concat hash function and re-hash it.
				CandidateStake::<T>::insert(
					candidate.clone(),
					staker.clone(),
					CandidateStakeInfo { stake: value.stake, checkpoint: FixedU128::zero() },
				);
				cursor = Some((candidate, staker)) // Return the processed key as the new cursor.
			} else {
				cursor = None; // No more items to process.
				break;
			}
		}
		match cursor {
			None => MigrationSteps::ResetClaimableRewards,
			Some(checkpoint) => MigrationSteps::MigrateStake { cursor: Some(checkpoint) },
		}
	}
}

impl<T: Config> SteppedMigration for LazyMigrationV2<T> {
	type Cursor = MigrationSteps<T>;
	// Without the explicit length here the construction of the ID would not be infallible.
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
		let cursor = maybe_cursor.unwrap_or_else(|| MigrationSteps::MigrateStake { cursor: None });
		let new_cursor = match cursor {
			MigrationSteps::MigrateStake { cursor: checkpoint } => Some(Self::migrate_stake(meter, checkpoint)),
			MigrationSteps::ResetClaimableRewards => Some(Self::reset_rewards(meter)),
			MigrationSteps::Noop => None,
		};

		Ok(new_cursor)
	}

	#[cfg(feature = "try-runtime")]
	fn pre_upgrade() -> Result<Vec<u8>, TryRuntimeError> {
		use codec::Encode;

		// Return the state of the storage before the migration.
		let map: BTreeMap<(T::AccountId, T::AccountId), v1::CandidateStakeInfo<BalanceOf<T>>> =
			v1::CandidateStake::<T>::iter().map(|(k1, k2, v)| ((k1, k2), v)).collect();
		Ok(map.encode())
	}

	#[cfg(feature = "try-runtime")]
	fn post_upgrade(prev: Vec<u8>) -> Result<(), TryRuntimeError> {
		use codec::Decode;

		// Check the state of the storage after the migration.
		let prev_map =
			BTreeMap::<(T::AccountId, T::AccountId), v1::CandidateStakeInfo<BalanceOf<T>>>::decode(
				&mut &prev[..],
			)
			.expect("Failed to decode the previous storage state");

		// Check the len of prev and post are the same.
		assert_eq!(prev_map.len(), CandidateStake::<T>::iter().count(), "Migration failed: the number of items in the storage after the migration is not the same as before");

		for ((candidate, staker), value) in prev_map {
			let new_value = CandidateStake::<T>::get(candidate, staker);
			assert_eq!(
				value.stake, new_value.stake,
				"Migration failed: the stake after the migration is not the same as before"
			);
			assert_eq!(
				new_value.checkpoint,
				FixedU128::zero(),
				"Migration failed: the checkpoint after the migration is not zero"
			);
		}

		Ok(())
	}
}

#[cfg(all(test, not(feature = "runtime-benchmarks")))]
mod tests {
	use super::*;
	use crate::mock::*;
	use frame_support::traits::OnRuntimeUpgrade;

	#[test]
	fn migration_of_single_element_should_work() {
		new_test_ext().execute_with(|| {
			initialize_to_block(1);
			v1::CandidateStake::<Test>::insert(
				&1,
				&1,
				v1::CandidateStakeInfo { session: 10, stake: 50 },
			);

			// Trigger the runtime upgrade
			AllPalletsWithSystem::on_runtime_upgrade();
			initialize_to_block(2);

			assert_eq!(
				CandidateStake::<Test>::get(&1, &1),
				CandidateStakeInfo { stake: 50, checkpoint: FixedU128::zero() }
			);
		});
	}

	#[test]
	fn migration_of_many_elements_should_work() {
		new_test_ext().execute_with(|| {
			initialize_to_block(1);

			for i in 1..=100 {
				v1::CandidateStake::<Test>::insert(
					&i,
					&i,
					v1::CandidateStakeInfo { session: 10, stake: 50 },
				);
			}

			// Trigger the runtime upgrade
			AllPalletsWithSystem::on_runtime_upgrade();
			initialize_to_block(2);

			for i in 1..=100 {
				assert_eq!(
					CandidateStake::<Test>::get(&i, &i),
					CandidateStakeInfo { stake: 50, checkpoint: FixedU128::zero() }
				);
			}
		});
	}
}
