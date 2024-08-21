//! Benchmarking setup for pallet-collator-staking

use super::*;

#[allow(unused)]
use crate::Pallet as CollatorStaking;
use codec::Decode;
use frame_benchmarking::{account, v2::*, whitelisted_caller, BenchmarkError};
use frame_support::traits::fungible::{Inspect, Mutate};
use frame_support::traits::{EnsureOrigin, Get};
use frame_system::{pallet_prelude::BlockNumberFor, EventRecord, RawOrigin};
use pallet_authorship::EventHandler;
use pallet_session::SessionManager;
use sp_runtime::traits::Zero;
use sp_runtime::Percent;
use sp_std::prelude::*;

const SEED: u32 = 0;

fn assert_last_event<T: Config>(generic_event: <T as Config>::RuntimeEvent) {
	let events = frame_system::Pallet::<T>::events();
	let system_event: <T as frame_system::Config>::RuntimeEvent = generic_event.into();
	// compare to the last event record
	let EventRecord { event, .. } = &events[events.len() - 1];
	assert_eq!(event, &system_event);
}

fn assert_has_event<T: Config>(generic_event: <T as Config>::RuntimeEvent) {
	let events = frame_system::Pallet::<T>::events();
	let system_event: <T as frame_system::Config>::RuntimeEvent = generic_event.into();

	assert!(
		events.iter().any(|record| record.event == system_event),
		"expected event {system_event:?} not found in events {events:?}"
	);
}

fn create_funded_user<T: Config>(
	string: &'static str,
	n: u32,
	balance_factor: u32,
) -> T::AccountId {
	let user = account(string, n, SEED);
	let balance = T::Currency::minimum_balance() * balance_factor.into();
	T::Currency::mint_into(&user, balance).unwrap();
	user
}

fn keys<T: Config + pallet_session::Config>(c: u32) -> <T as pallet_session::Config>::Keys {
	use rand::{RngCore, SeedableRng};

	let keys = {
		let mut keys = [0u8; 128];

		if c > 0 {
			let mut rng = rand::rngs::StdRng::seed_from_u64(c as u64);
			rng.fill_bytes(&mut keys);
		}

		keys
	};

	Decode::decode(&mut &keys[..]).unwrap()
}

fn validator<T: Config + pallet_session::Config>(
	c: u32,
) -> (T::AccountId, <T as pallet_session::Config>::Keys) {
	(create_funded_user::<T>("candidate", c, 1000), keys::<T>(c))
}

fn register_validators<T: Config + pallet_session::Config>(count: u32) -> Vec<T::AccountId> {
	let validators = (0..count).map(|c| validator::<T>(c)).collect::<Vec<_>>();

	for (who, keys) in validators.clone() {
		pallet_session::Pallet::<T>::set_keys(RawOrigin::Signed(who).into(), keys, Vec::new())
			.unwrap();
	}

	validators.into_iter().map(|(who, _)| who).collect()
}

fn register_candidates<T: Config>(count: u32) {
	let candidates = (0..count).map(|c| account("candidate", c, SEED)).collect::<Vec<_>>();
	assert!(MinCandidacyBond::<T>::get() > 0u32.into(), "Bond cannot be zero!");

	for who in candidates {
		T::Currency::mint_into(&who, MinCandidacyBond::<T>::get() * 3u32.into()).unwrap();
		CollatorStaking::<T>::register_as_candidate(
			RawOrigin::Signed(who).into(),
			MinCandidacyBond::<T>::get(),
		)
		.unwrap();
	}
}

fn min_candidates<T: Config>() -> u32 {
	let min_collators = T::MinEligibleCollators::get();
	let invulnerable_length = Invulnerables::<T>::get().len();
	min_collators.saturating_sub(invulnerable_length.try_into().unwrap())
}

fn min_invulnerables<T: Config>() -> u32 {
	let min_collators = T::MinEligibleCollators::get();
	let candidates_length = Candidates::<T>::count();
	min_collators.saturating_sub(candidates_length)
}

#[benchmarks(where T: pallet_authorship::Config + pallet_session::Config)]
mod benchmarks {
	use super::*;
	use frame_support::traits::fungible::{Inspect, InspectFreeze, Mutate};
	use frame_support::BoundedBTreeMap;

	#[benchmark]
	fn set_invulnerables(
		b: Linear<1, { T::MaxInvulnerables::get() }>,
	) -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;

		let new_invulnerables = register_validators::<T>(b);
		let mut sorted_new_invulnerables = new_invulnerables.clone();
		sorted_new_invulnerables.sort();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, new_invulnerables.clone());

		// assert that it comes out sorted
		assert_last_event::<T>(
			Event::NewInvulnerables { invulnerables: sorted_new_invulnerables }.into(),
		);
		Ok(())
	}

	#[benchmark]
	fn add_invulnerable(
		b: Linear<1, { T::MaxInvulnerables::get() - 1 }>,
	) -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;

		// need to fill up candidates
		MinCandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(0);
		// add one more to the list. should not be in `b` (invulnerables) because it's the account
		// we will _add_ to invulnerables. we want it to be in `candidates` because we need the
		// weight associated with removing it.
		let (new_invulnerable, keys) = validator::<T>(b + 1);

		// now we need to fill up invulnerables
		let mut invulnerables = register_validators::<T>(b);
		invulnerables.sort();
		let invulnerables: frame_support::BoundedVec<_, T::MaxInvulnerables> =
			frame_support::BoundedVec::try_from(invulnerables).unwrap();
		Invulnerables::<T>::put(invulnerables);
		pallet_session::Pallet::<T>::set_keys(
			RawOrigin::Signed(new_invulnerable.clone()).into(),
			keys,
			Vec::new(),
		)
		.unwrap();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, new_invulnerable.clone());

		assert_last_event::<T>(Event::InvulnerableAdded { account: new_invulnerable }.into());
		Ok(())
	}

	#[benchmark]
	fn remove_invulnerable(
		b: Linear<{ min_invulnerables::<T>() + 1 }, { T::MaxInvulnerables::get() }>,
	) -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let mut invulnerables = register_validators::<T>(b);
		invulnerables.sort();
		let invulnerables: frame_support::BoundedVec<_, T::MaxInvulnerables> =
			frame_support::BoundedVec::try_from(invulnerables).unwrap();
		Invulnerables::<T>::put(invulnerables);
		let to_remove = Invulnerables::<T>::get().first().unwrap().clone();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, to_remove.clone());

		assert_last_event::<T>(Event::InvulnerableRemoved { account_id: to_remove }.into());
		Ok(())
	}

	#[benchmark]
	fn set_desired_candidates() -> Result<(), BenchmarkError> {
		let max: u32 = T::MaxCandidates::get();
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, max);

		assert_last_event::<T>(Event::NewDesiredCandidates { desired_candidates: max }.into());
		Ok(())
	}

	#[benchmark]
	fn set_min_candidacy_bond() -> Result<(), BenchmarkError> {
		let initial_bond_amount: BalanceOf<T> = T::Currency::minimum_balance() * 2u32.into();
		MinCandidacyBond::<T>::put(initial_bond_amount);
		MinStake::<T>::put(T::Currency::minimum_balance());

		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let bond_amount = MinStake::<T>::get();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, bond_amount);

		assert_last_event::<T>(Event::NewMinCandidacyBond { bond_amount }.into());
		Ok(())
	}

	// worse case is when we have all the max-candidate slots filled except one, and we fill that
	// one.
	#[benchmark]
	fn register_as_candidate() -> Result<(), BenchmarkError> {
		MinCandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(1);

		let caller: T::AccountId = whitelisted_caller();
		let bond: BalanceOf<T> = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::mint_into(&caller, bond)?;

		pallet_session::Pallet::<T>::set_keys(
			RawOrigin::Signed(caller.clone()).into(),
			keys::<T>(1),
			Vec::new(),
		)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), bond);

		assert_last_event::<T>(Event::CandidateAdded { account: caller, deposit: bond }.into());
		Ok(())
	}

	#[benchmark]
	fn remove_worst_candidate() -> Result<(), BenchmarkError> {
		let min_bond = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(min_bond);

		// Fill the candidates.
		let candidates = <T as Config>::MaxCandidates::get();
		let validators = register_validators::<T>(candidates);
		let worst_validator = validators[0].clone();
		register_candidates::<T>(candidates);
		for i in 1..candidates {
			CollatorStaking::<T>::update_candidacy_bond(
				RawOrigin::Signed(validators[i as usize].clone()).into(),
				min_bond * 2u32.into(),
			)?;
		}

		#[block]
		{
			CollatorStaking::<T>::remove_worst_candidate(
				T::Currency::minimum_balance() * 3u32.into(),
			)?;
		}

		assert_last_event::<T>(Event::CandidateRemoved { account: worst_validator }.into());
		Ok(())
	}

	#[benchmark]
	fn leave_intent() {
		MinCandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(1);

		register_validators::<T>(1);
		register_candidates::<T>(1);

		let leaving = Candidates::<T>::iter().next().unwrap().0.clone();
		v2::whitelist!(leaving);

		#[extrinsic_call]
		_(RawOrigin::Signed(leaving.clone()));

		assert_last_event::<T>(Event::CandidateRemoved { account: leaving }.into());
	}

	// worse case is paying a non-existing candidate account.
	#[benchmark]
	fn note_author() {
		let author: T::AccountId = account("author", 0, SEED);
		let new_block: BlockNumberFor<T> = 10u32.into();

		frame_system::Pallet::<T>::set_block_number(new_block);
		#[block]
		{
			<CollatorStaking<T> as EventHandler<_, _>>::note_author(author.clone())
		}

		assert_eq!(LastAuthoredBlock::<T>::get(&author), new_block);
		assert_eq!(frame_system::Pallet::<T>::block_number(), new_block);
	}

	// worst case for new session.
	#[benchmark]
	fn new_session(
		r: Linear<1, { T::MaxCandidates::get() }>,
		c: Linear<1, { T::MaxCandidates::get() }>,
	) {
		MinCandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(c);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let new_block: BlockNumberFor<T> = T::KickThreshold::get();
		let zero_block: BlockNumberFor<T> = 0u32.into();
		let candidates: Vec<T::AccountId> = Candidates::<T>::iter_keys().collect();

		let non_removals = c.saturating_sub(r);

		for i in 0..c {
			LastAuthoredBlock::<T>::insert(candidates[i as usize].clone(), zero_block);
		}

		if non_removals > 0 {
			for i in 0..non_removals {
				LastAuthoredBlock::<T>::insert(candidates[i as usize].clone(), new_block);
			}
		} else {
			for i in 0..c {
				LastAuthoredBlock::<T>::insert(candidates[i as usize].clone(), new_block);
			}
		}

		let min_candidates = min_candidates::<T>();
		let pre_length = Candidates::<T>::count();

		frame_system::Pallet::<T>::set_block_number(new_block);

		let current_length = Candidates::<T>::count();
		assert!(c == current_length);
		#[block]
		{
			<CollatorStaking<T> as SessionManager<_>>::new_session(0);
		}

		if c > r && non_removals >= min_candidates {
			// candidates > removals and remaining candidates > min candidates
			// => remaining candidates should be shorter than before removal, i.e. some were
			//    actually removed.
			assert!(Candidates::<T>::count() < pre_length);
		} else if c > r && non_removals < min_candidates {
			// candidates > removals and remaining candidates would be less than min candidates
			// => remaining candidates should equal min candidates, i.e. some were removed up to
			//    the minimum, but then anymore were "forced" to stay in candidates.
			let current_length: u32 = Candidates::<T>::count();
			assert_eq!(min_candidates, current_length);
		} else {
			// removals >= candidates, non removals must == 0
			// can't remove more than exist
			assert_eq!(Candidates::<T>::count(), pre_length);
		}
	}

	#[benchmark]
	fn stake(c: Linear<1, { T::MaxStakedCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let caller: T::AccountId = whitelisted_caller();
		T::Currency::mint_into(&caller, T::Currency::minimum_balance() * 2u32.into() * c.into())
			.unwrap();
		CollatorStaking::<T>::lock(
			RawOrigin::Signed(caller.clone()).into(),
			CollatorStaking::<T>::get_free_balance(&caller),
		)
		.unwrap();
		let targets = Candidates::<T>::iter_keys()
			.map(|candidate| StakeTarget { candidate, stake: amount })
			.collect::<Vec<_>>()
			.try_into()
			.unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), targets);

		for candidate in Candidates::<T>::iter_keys() {
			assert_eq!(CandidateStake::<T>::get(&candidate, &caller).stake, amount);
		}
	}

	// worst case is promoting from last position to first one
	#[benchmark]
	fn unstake_from() {
		let amount = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(1);
		register_candidates::<T>(1);
		let candidate = Candidates::<T>::iter_keys().next().unwrap().clone();
		CollatorStaking::<T>::lock(
			RawOrigin::Signed(candidate.clone()).into(),
			CollatorStaking::<T>::get_free_balance(&candidate),
		)
		.unwrap();
		CollatorStaking::<T>::stake(
			RawOrigin::Signed(candidate.clone()).into(),
			vec![StakeTarget { candidate: candidate.clone(), stake: amount }]
				.try_into()
				.unwrap(),
		)
		.unwrap();
		assert_eq!(CandidateStake::<T>::get(&candidate, &candidate).stake, amount);

		whitelist_account!(candidate);

		#[extrinsic_call]
		_(RawOrigin::Signed(candidate.clone()), candidate.clone());

		assert_eq!(CandidateStake::<T>::get(&candidate, &candidate).stake, 0u32.into());
	}

	// worst case is having stake in as many collators as possible
	#[benchmark]
	fn unstake_all(s: Linear<1, { T::MaxStakedCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(s);
		register_candidates::<T>(s);

		let caller: T::AccountId = whitelisted_caller();
		let balance = amount * 2u32.into() * s.into();
		T::Currency::mint_into(&caller, balance).unwrap();
		CollatorStaking::<T>::lock(
			RawOrigin::Signed(caller.clone()).into(),
			CollatorStaking::<T>::get_free_balance(&caller),
		)
		.unwrap();
		Candidates::<T>::iter_keys().for_each(|who| {
			CollatorStaking::<T>::stake(
				RawOrigin::Signed(caller.clone()).into(),
				vec![StakeTarget { candidate: who.clone(), stake: amount }].try_into().unwrap(),
			)
			.unwrap();
			assert_eq!(CandidateStake::<T>::get(&who, &caller).stake, amount);
		});

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()));

		Candidates::<T>::iter().for_each(|(who, info)| {
			assert_eq!(CandidateStake::<T>::get(&who, &caller).stake, 0u32.into());
			assert_eq!(info.stake, 0u32.into());
		});
	}

	#[benchmark]
	fn release(c: Linear<1, { T::MaxStakedCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		let caller = whitelisted_caller();
		T::Currency::mint_into(&caller, amount * 2u32.into() * (c + 1).into()).unwrap();
		CollatorStaking::<T>::lock(
			RawOrigin::Signed(caller.clone()).into(),
			CollatorStaking::<T>::get_free_balance(&caller),
		)
		.unwrap();
		for _ in 0..c {
			CollatorStaking::<T>::unlock(
				RawOrigin::Signed(caller.clone()).into(),
				Some(1u32.into()),
			)
			.unwrap();
		}
		assert_eq!(c as usize, ReleaseQueues::<T>::get(&caller).len());
		frame_system::Pallet::<T>::set_block_number(100u32.into());

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()));

		assert_eq!(0, ReleaseQueues::<T>::get(&caller).len());
	}

	#[benchmark]
	fn claim_rewards(
		c: Linear<1, { T::MaxStakedCandidates::get() }>,
		r: Linear<1, { T::MaxSessionRewards::get() }>,
	) {
		let amount = T::Currency::minimum_balance();
		MinStake::<T>::put(amount);
		MinCandidacyBond::<T>::put(amount);
		let staker = create_funded_user::<T>("staker", 0, 10000);
		CollatorStaking::<T>::lock(
			RawOrigin::Signed(staker.clone()).into(),
			CollatorStaking::<T>::get_free_balance(&staker),
		)
		.unwrap();
		CollatorStaking::<T>::set_autocompound_percentage(
			RawOrigin::Signed(staker.clone()).into(),
			Percent::from_parts(100),
		)
		.unwrap();

		let mut reward_map = BoundedBTreeMap::new();
		let total_candidates = T::MaxCandidates::get();
		let candidates = register_validators::<T>(total_candidates);
		register_candidates::<T>(total_candidates);
		for (index, candidate) in candidates.iter().enumerate() {
			if index < (c as usize) {
				CollatorStaking::<T>::stake(
					RawOrigin::Signed(staker.clone()).into(),
					vec![StakeTarget { candidate: candidate.clone(), stake: amount }]
						.try_into()
						.unwrap(),
				)
				.unwrap_or_else(|e| panic!("Could not stake: {:?}", e));
			}
			reward_map.try_insert(candidate.clone(), (amount, amount)).unwrap();
		}

		for session in 1..(r + 1) {
			PerSessionRewards::<T>::insert(
				session,
				SessionInfo { candidates: reward_map.clone(), rewards: amount * c.into() },
			);
		}

		let total_rewards = amount * c.into() * r.into();
		ClaimableRewards::<T>::set(total_rewards);
		CurrentSession::<T>::mutate(|session| *session = r + 2);
		T::Currency::mint_into(
			&CollatorStaking::<T>::account_id(),
			T::Currency::minimum_balance() + total_rewards,
		)
		.unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(staker.clone()));

		assert_has_event::<T>(
			Event::<T>::StakingRewardReceived { account: staker.clone(), amount: total_rewards }
				.into(),
		);
		for candidate in &candidates[..(c as usize)] {
			assert_has_event::<T>(
				Event::<T>::StakeAdded {
					account: staker.clone(),
					candidate: candidate.clone(),
					amount: total_rewards / c.into(),
				}
				.into(),
			);
		}
	}

	#[benchmark]
	fn set_autocompound_percentage() {
		let caller: T::AccountId = whitelisted_caller();
		let percent = Percent::from_parts(50);

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), percent);

		assert_eq!(AutoCompound::<T>::get(&caller), percent);
	}

	#[benchmark]
	fn set_collator_reward_percentage() -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let percent = Percent::from_parts(70);

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, percent);

		assert_eq!(CollatorRewardPercentage::<T>::get(), percent);
		Ok(())
	}

	#[benchmark]
	fn set_extra_reward() -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let extra_reward = 5u32.into();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, extra_reward);

		assert_eq!(ExtraReward::<T>::get(), extra_reward);
		Ok(())
	}

	#[benchmark]
	fn set_minimum_stake() -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let min_stake = 3u32.into();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, min_stake);

		assert_eq!(MinStake::<T>::get(), min_stake);
		Ok(())
	}

	#[benchmark]
	fn stop_extra_reward() -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let initial_reward: BalanceOf<T> = T::Currency::minimum_balance();
		ExtraReward::<T>::put(initial_reward);
		T::Currency::mint_into(&CollatorStaking::<T>::extra_reward_account_id(), initial_reward)?;

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin);

		assert_eq!(ExtraReward::<T>::get(), 0u32.into());
		Ok(())
	}

	#[benchmark]
	fn top_up_extra_rewards() -> Result<(), BenchmarkError> {
		let caller: T::AccountId = whitelisted_caller();
		let balance = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::mint_into(&caller, balance).unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), T::Currency::minimum_balance());

		assert_eq!(
			T::Currency::balance(&CollatorStaking::<T>::extra_reward_account_id()),
			T::Currency::minimum_balance()
		);
		Ok(())
	}

	#[benchmark]
	fn start_session() {
		#[block]
		{
			<CollatorStaking<T> as SessionManager<_>>::start_session(1);
		}

		assert_eq!(TotalBlocks::<T>::get(), (0, 0));
		assert_eq!(CurrentSession::<T>::get(), 1);
	}

	#[benchmark]
	fn end_session(c: Linear<1, { T::MaxCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		MinCandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		CollatorRewardPercentage::<T>::put(Percent::from_parts(20));
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		let candidates = register_validators::<T>(c);
		register_candidates::<T>(c);

		<CollatorStaking<T> as SessionManager<_>>::start_session(1);
		for candidate in &candidates {
			<CollatorStaking<T> as EventHandler<_, _>>::note_author(candidate.clone())
		}
		frame_system::Pallet::<T>::set_block_number(20u32.into());
		let total_rewards = amount * c.into();
		T::Currency::mint_into(
			&CollatorStaking::<T>::account_id(),
			total_rewards + T::Currency::minimum_balance(),
		)
		.unwrap();
		for (n, candidate) in candidates.iter().enumerate() {
			let staker = create_funded_user::<T>("staker", n as u32, 1000);
			CollatorStaking::<T>::lock(
				RawOrigin::Signed(staker.clone()).into(),
				CollatorStaking::<T>::get_free_balance(&staker),
			)
			.unwrap();
			CollatorStaking::<T>::stake(
				RawOrigin::Signed(staker.clone()).into(),
				vec![StakeTarget { candidate: candidate.clone(), stake: amount }]
					.try_into()
					.unwrap(),
			)
			.unwrap();
		}

		#[block]
		{
			<CollatorStaking<T> as SessionManager<_>>::end_session(1);
		}

		let collator_reward = CollatorRewardPercentage::<T>::get().mul_floor(amount);
		if !collator_reward.is_zero() {
			for candidate in candidates {
				assert_has_event::<T>(
					Event::<T>::StakingRewardReceived {
						account: candidate.clone(),
						amount: collator_reward,
					}
					.into(),
				);
			}
		}
	}

	#[benchmark]
	fn update_candidacy_bond() {
		MinCandidacyBond::<T>::put(T::Currency::minimum_balance());
		let caller = register_validators::<T>(1)[0].clone();
		whitelist_account!(caller);
		let balance = MinCandidacyBond::<T>::get() * 2u32.into();
		T::Currency::mint_into(&caller, balance).unwrap();

		CollatorStaking::<T>::register_as_candidate(
			RawOrigin::Signed(caller.clone()).into(),
			MinCandidacyBond::<T>::get(),
		)
		.unwrap();
		assert_eq!(
			T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &caller),
			MinCandidacyBond::<T>::get()
		);

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), balance);

		assert_eq!(
			T::Currency::balance_frozen(&FreezeReason::CandidacyBond.into(), &caller),
			balance
		);
	}

	#[benchmark]
	fn lock() {
		let caller: T::AccountId = whitelisted_caller();
		let balance = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::mint_into(&caller, balance).unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), balance);

		assert_eq!(T::Currency::balance_frozen(&FreezeReason::Staking.into(), &caller), balance);
	}

	#[benchmark]
	fn unlock() {
		let caller: T::AccountId = whitelisted_caller();
		let balance = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::mint_into(&caller, balance).unwrap();

		CollatorStaking::<T>::lock(RawOrigin::Signed(caller.clone()).into(), balance).unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), Some(T::Currency::minimum_balance()));

		assert_eq!(
			T::Currency::balance_frozen(&FreezeReason::Staking.into(), &caller),
			T::Currency::minimum_balance()
		);
	}

	impl_benchmark_test_suite!(CollatorStaking, crate::mock::new_test_ext(), crate::mock::Test,);
}
