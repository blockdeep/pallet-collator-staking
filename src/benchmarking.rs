//! Benchmarking setup for pallet-collator-selection

#![cfg(feature = "runtime-benchmarks")]

use super::*;

#[allow(unused)]
use crate::Pallet as CollatorStaking;
use codec::Decode;
use frame_benchmarking::{account, v2::*, whitelisted_caller, BenchmarkError};
use frame_support::pallet_prelude::Weight;
use frame_support::traits::{Currency, EnsureOrigin, Get, ReservableCurrency};
use frame_system::{pallet_prelude::BlockNumberFor, EventRecord, RawOrigin};
use pallet_authorship::EventHandler;
use pallet_session::SessionManager;
use sp_runtime::Percent;
use sp_std::prelude::*;

pub type BalanceOf<T> =
	<<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

const SEED: u32 = 0;

fn assert_last_event<T: Config>(generic_event: <T as Config>::RuntimeEvent) {
	let events = frame_system::Pallet::<T>::events();
	let system_event: <T as frame_system::Config>::RuntimeEvent = generic_event.into();
	// compare to the last event record
	let EventRecord { event, .. } = &events[events.len() - 1];
	assert_eq!(event, &system_event);
}

fn create_funded_user<T: Config>(
	string: &'static str,
	n: u32,
	balance_factor: u32,
) -> T::AccountId {
	let user = account(string, n, SEED);
	let balance = T::Currency::minimum_balance() * balance_factor.into();
	let _ = T::Currency::make_free_balance_be(&user, balance);
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
	assert!(CandidacyBond::<T>::get() > 0u32.into(), "Bond cannot be zero!");

	for who in candidates {
		T::Currency::make_free_balance_be(&who, CandidacyBond::<T>::get() * 3u32.into());
		CollatorStaking::<T>::register_as_candidate(RawOrigin::Signed(who).into()).unwrap();
	}
}

fn min_candidates<T: Config>() -> u32 {
	let min_collators = T::MinEligibleCollators::get();
	let invulnerable_length = Invulnerables::<T>::get().len();
	min_collators.saturating_sub(invulnerable_length.try_into().unwrap())
}

fn min_invulnerables<T: Config>() -> u32 {
	let min_collators = T::MinEligibleCollators::get();
	let candidates_length = CandidateList::<T>::decode_len()
		.unwrap_or_default()
		.try_into()
		.unwrap_or_default();
	min_collators.saturating_sub(candidates_length)
}

#[benchmarks(where T: pallet_authorship::Config + pallet_session::Config)]
mod benchmarks {
	use super::*;

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
		c: Linear<1, { T::MaxCandidates::get() - 1 }>,
	) -> Result<(), BenchmarkError> {
		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;

		// need to fill up candidates
		CandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(c);
		// get accounts and keys for the `c` candidates
		let mut candidates = (0..c).map(|cc| validator::<T>(cc)).collect::<Vec<_>>();
		// add one more to the list. should not be in `b` (invulnerables) because it's the account
		// we will _add_ to invulnerables. we want it to be in `candidates` because we need the
		// weight associated with removing it.
		let (new_invulnerable, new_invulnerable_keys) = validator::<T>(b.max(c) + 1);
		candidates.push((new_invulnerable.clone(), new_invulnerable_keys));
		// set their keys ...
		for (who, keys) in candidates.clone() {
			pallet_session::Pallet::<T>::set_keys(RawOrigin::Signed(who).into(), keys, Vec::new())
				.unwrap();
		}
		// ... and register them.
		for (who, _) in candidates.iter() {
			let deposit = CandidacyBond::<T>::get();
			T::Currency::make_free_balance_be(who, deposit * 1000_u32.into());
			CandidateList::<T>::try_mutate(|list| {
				list.try_push(CandidateInfo { who: who.clone(), deposit, stakers: 1 }).unwrap();
				Ok::<(), BenchmarkError>(())
			})
			.unwrap();
			T::Currency::reserve(who, deposit)?;
			LastAuthoredBlock::<T>::insert(
				who.clone(),
				frame_system::Pallet::<T>::block_number() + T::KickThreshold::get(),
			);
		}

		// now we need to fill up invulnerables
		let mut invulnerables = register_validators::<T>(b);
		invulnerables.sort();
		let invulnerables: frame_support::BoundedVec<_, T::MaxInvulnerables> =
			frame_support::BoundedVec::try_from(invulnerables).unwrap();
		Invulnerables::<T>::put(invulnerables);

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, new_invulnerable.clone());

		assert_last_event::<T>(Event::InvulnerableAdded { account_id: new_invulnerable }.into());
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
	fn set_candidacy_bond() -> Result<(), BenchmarkError> {
		let initial_bond_amount: BalanceOf<T> = T::Currency::minimum_balance() * 2u32.into();
		CandidacyBond::<T>::put(initial_bond_amount);
		MinStake::<T>::put(T::Currency::minimum_balance());

		let origin =
			T::UpdateOrigin::try_successful_origin().map_err(|_| BenchmarkError::Weightless)?;
		let bond_amount = MinStake::<T>::get();

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, bond_amount);

		assert_last_event::<T>(Event::NewCandidacyBond { bond_amount }.into());
		Ok(())
	}

	// worse case is when we have all the max-candidate slots filled except one, and we fill that
	// one.
	#[benchmark]
	fn register_as_candidate(c: Linear<1, { T::MaxCandidates::get() - 1 }>) {
		CandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(c + 1);

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let caller: T::AccountId = whitelisted_caller();
		let bond: BalanceOf<T> = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::make_free_balance_be(&caller, bond);

		pallet_session::Pallet::<T>::set_keys(
			RawOrigin::Signed(caller.clone()).into(),
			keys::<T>(c + 1),
			Vec::new(),
		)
		.unwrap();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()));

		assert_last_event::<T>(
			Event::CandidateAdded { account_id: caller, deposit: bond / 2u32.into() }.into(),
		);
	}

	#[benchmark]
	fn take_candidate_slot() {
		CandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(1);

		let c = T::MaxCandidates::get();
		register_validators::<T>(c);
		register_candidates::<T>(c);

		let caller: T::AccountId = whitelisted_caller();
		let bond: BalanceOf<T> = T::Currency::minimum_balance() * 10u32.into();
		T::Currency::make_free_balance_be(&caller, bond);

		pallet_session::Pallet::<T>::set_keys(
			RawOrigin::Signed(caller.clone()).into(),
			keys::<T>(c + 1),
			Vec::new(),
		)
		.unwrap();

		let target = CandidateList::<T>::get().iter().last().unwrap().who.clone();

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), bond / 2u32.into(), target.clone());

		assert_last_event::<T>(
			Event::CandidateReplaced { old: target, new: caller, deposit: bond / 2u32.into() }
				.into(),
		);
	}

	// worse case is the last candidate leaving.
	#[benchmark]
	fn leave_intent(c: Linear<{ min_candidates::<T>() + 1 }, { T::MaxCandidates::get() }>) {
		CandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(c);

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let leaving = CandidateList::<T>::get().iter().last().unwrap().who.clone();
		v2::whitelist!(leaving);

		#[extrinsic_call]
		_(RawOrigin::Signed(leaving.clone()));

		assert_last_event::<T>(Event::CandidateRemoved { account_id: leaving }.into());
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
		CandidacyBond::<T>::put(T::Currency::minimum_balance());
		MinStake::<T>::put(T::Currency::minimum_balance());
		DesiredCandidates::<T>::put(c);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let new_block: BlockNumberFor<T> = T::KickThreshold::get();
		let zero_block: BlockNumberFor<T> = 0u32.into();
		let candidates: Vec<T::AccountId> = CandidateList::<T>::get()
			.iter()
			.map(|candidate_info| candidate_info.who.clone())
			.collect();

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
		let pre_length = CandidateList::<T>::decode_len().unwrap_or_default();

		frame_system::Pallet::<T>::set_block_number(new_block);

		let current_length: u32 = CandidateList::<T>::decode_len()
			.unwrap_or_default()
			.try_into()
			.unwrap_or_default();
		assert!(c == current_length);
		#[block]
		{
			<CollatorStaking<T> as SessionManager<_>>::new_session(0);
		}

		if c > r && non_removals >= min_candidates {
			// candidates > removals and remaining candidates > min candidates
			// => remaining candidates should be shorter than before removal, i.e. some were
			//    actually removed.
			assert!(CandidateList::<T>::decode_len().unwrap_or_default() < pre_length);
		} else if c > r && non_removals < min_candidates {
			// candidates > removals and remaining candidates would be less than min candidates
			// => remaining candidates should equal min candidates, i.e. some were removed up to
			//    the minimum, but then any more were "forced" to stay in candidates.
			let current_length: u32 = CandidateList::<T>::decode_len()
				.unwrap_or_default()
				.try_into()
				.unwrap_or_default();
			assert_eq!(min_candidates, current_length);
		} else {
			// removals >= candidates, non removals must == 0
			// can't remove more than exist
			assert_eq!(CandidateList::<T>::decode_len().unwrap_or_default(), pre_length);
		}
	}

	// worst case is promoting from first position to last one.
	#[benchmark]
	fn stake(c: Linear<1, { T::MaxCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		CandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let candidate = CandidateList::<T>::get()[0].who.clone();
		whitelist_account!(candidate);
		let stake_before = Stake::<T>::get(&candidate, &candidate);

		#[extrinsic_call]
		_(RawOrigin::Signed(candidate.clone()), candidate.clone(), amount);

		assert_eq!(Stake::<T>::get(&candidate, &candidate), stake_before + amount);
		assert_eq!(&CandidateList::<T>::get()[(c - 1) as usize].who, &candidate);
	}

	// worst case is promoting from last position to first one
	#[benchmark]
	fn unstake_from(c: Linear<1, { T::MaxCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		CandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let candidate = CandidateList::<T>::get()[(c - 1) as usize].who.clone();
		whitelist_account!(candidate);

		#[extrinsic_call]
		_(RawOrigin::Signed(candidate.clone()), candidate.clone());

		assert_eq!(Stake::<T>::get(&candidate, &candidate), 0u32.into());
		assert_eq!(&CandidateList::<T>::get()[0].who, &candidate);
	}

	// worst case is having stake in as many collators as possible
	#[benchmark]
	fn unstake_all(
		c: Linear<{ T::MaxStakedCandidates::get() }, { T::MaxCandidates::get() }>,
		s: Linear<1, { T::MaxStakedCandidates::get() }>,
	) {
		let amount = T::Currency::minimum_balance();
		CandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let caller = whitelisted_caller();
		T::Currency::make_free_balance_be(&caller, amount * 2u32.into() * c.into());
		CandidateList::<T>::get().iter().take(s as usize).for_each(|cand| {
			assert_eq!(cand.deposit, amount);
			CollatorStaking::<T>::stake(
				RawOrigin::Signed(caller.clone()).into(),
				cand.who.clone(),
				amount,
			)
			.unwrap();
			assert_eq!(Stake::<T>::get(&cand.who, &cand.who), amount);
			assert_eq!(Stake::<T>::get(&cand.who, &caller), amount);
		});

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()));

		CandidateList::<T>::get().iter().for_each(|cand| {
			assert_eq!(Stake::<T>::get(&cand.who, &cand.who), amount);
			assert_eq!(Stake::<T>::get(&cand.who, &caller), 0u32.into());
			assert_eq!(cand.deposit, amount);
		});
	}

	#[benchmark]
	fn claim(c: Linear<0, { T::MaxStakedCandidates::get() }>) {
		let amount = T::Currency::minimum_balance();
		CandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());

		register_validators::<T>(c);
		register_candidates::<T>(c);

		let caller = whitelisted_caller();
		T::Currency::make_free_balance_be(&caller, amount * 2u32.into() * c.into());
		CandidateList::<T>::get().iter().for_each(|cand| {
			CollatorStaking::<T>::stake(
				RawOrigin::Signed(caller.clone()).into(),
				cand.who.clone(),
				amount,
			)
			.unwrap();
		});

		CollatorStaking::<T>::unstake_all(RawOrigin::Signed(caller.clone()).into()).unwrap();
		assert_eq!(c as usize, UnstakingRequests::<T>::get(&caller).len());
		frame_system::Pallet::<T>::set_block_number(100u32.into());

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()));

		assert_eq!(0, UnstakingRequests::<T>::get(&caller).len());
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
		let initial_reward: BalanceOf<T> = 10u32.into();
		ExtraReward::<T>::put(initial_reward);
		let _ = T::Currency::make_free_balance_be(
			&CollatorStaking::<T>::extra_reward_account_id(),
			100u32.into(),
		);

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin);

		assert_eq!(ExtraReward::<T>::get(), 0u32.into());
		Ok(())
	}

	#[benchmark]
	fn top_up_extra_rewards() -> Result<(), BenchmarkError> {
		let caller: T::AccountId = whitelisted_caller();
		let balance: BalanceOf<T> = T::Currency::minimum_balance() * 2u32.into();
		T::Currency::make_free_balance_be(&caller, balance);

		#[extrinsic_call]
		_(RawOrigin::Signed(caller.clone()), T::Currency::minimum_balance());

		assert_eq!(
			T::Currency::free_balance(&CollatorStaking::<T>::extra_reward_account_id()),
			T::Currency::minimum_balance()
		);
		Ok(())
	}

	#[benchmark]
	fn reward_one_collator(
		c: Linear<1, { T::MaxStakedCandidates::get() }>,
		s: Linear<1, 1000>,
		a: Linear<0, 100>,
	) {
		let amount = T::Currency::minimum_balance();
		CandidacyBond::<T>::put(amount);
		MinStake::<T>::put(amount);
		frame_system::Pallet::<T>::set_block_number(0u32.into());
		CollatorRewardPercentage::<T>::put(Percent::from_parts(20));

		let collator = register_validators::<T>(c)[0].clone();
		register_candidates::<T>(c);

		let autocompound = Percent::from_parts(a as u8) * s;
		for n in 0..s {
			let acc = create_funded_user::<T>("staker", s, 1000);
			CollatorStaking::<T>::stake(
				RawOrigin::Signed(acc.clone()).into(),
				collator.clone(),
				amount,
			)
			.unwrap();
			if n <= autocompound {
				CollatorStaking::<T>::set_autocompound_percentage(
					RawOrigin::Signed(acc.clone()).into(),
					Percent::from_parts(50),
				)
				.unwrap();
			}
		}
		<CollatorStaking<T> as SessionManager<_>>::start_session(0);
		for _ in 0..10 {
			<CollatorStaking<T> as EventHandler<_, _>>::note_author(collator.clone())
		}
		frame_system::Pallet::<T>::set_block_number(10u32.into());
		let _ =
			T::Currency::make_free_balance_be(&CollatorStaking::<T>::account_id(), 100u32.into());
		<CollatorStaking<T> as SessionManager<_>>::end_session(0);
		<CollatorStaking<T> as SessionManager<_>>::start_session(1);

		#[block]
		{
			CollatorStaking::<T>::reward_one_collator(0, Weight::MAX);
		}
	}

	impl_benchmark_test_suite!(CollatorStaking, crate::mock::new_test_ext(), crate::mock::Test,);
}
