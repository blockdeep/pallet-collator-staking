use crate as collator_staking;
use crate::{
	mock::*, AutoCompound, CandidacyBond, CandidateInfo, CandidateList, CollatorRewardPercentage,
	Config, CurrentSession, DesiredCandidates, Error, Event, ExtraReward, Invulnerables,
	LastAuthoredBlock, MaxDesiredCandidates, MinStake, ProducedBlocks, StakeCount, TotalBlocks,
};
use crate::{Stake, UnstakeRequest, UnstakingRequests};
use frame_support::pallet_prelude::TypedGet;
use frame_support::traits::ExistenceRequirement::KeepAlive;
use frame_support::{
	assert_noop, assert_ok,
	traits::{Currency, OnInitialize},
};
use pallet_balances::Error as BalancesError;
use sp_runtime::{testing::UintAuthorityId, traits::BadOrigin, BuildStorage, Percent};
use std::ops::RangeInclusive;

type AccountId = <Test as frame_system::Config>::AccountId;

fn fund_account(acc: AccountId) {
	Balances::make_free_balance_be(&acc, 100);
}

fn register_keys(acc: AccountId) {
	let key = MockSessionKeys { aura: UintAuthorityId(acc) };
	Session::set_keys(RuntimeOrigin::signed(acc).into(), key, Vec::new()).unwrap();
}

fn register_candidates(range: RangeInclusive<AccountId>) {
	let candidacy_bond = CandidacyBond::<Test>::get();
	for ii in range {
		if ii > 5 {
			// only keys were registered in mock for 1 to 5
			fund_account(ii);
			register_keys(ii);
		}
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(ii),));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeAdded {
			staker: ii,
			candidate: ii,
			amount: 10,
		}));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::CandidateAdded {
			account_id: ii,
			deposit: 10,
		}));
		assert_eq!(Stake::<Test>::get(ii, ii), candidacy_bond);
	}
}

#[test]
fn basic_setup_works() {
	new_test_ext().execute_with(|| {
		assert_eq!(<Test as Config>::MaxInvulnerables::get(), 20);
		assert_eq!(<Test as Config>::MaxCandidates::get(), 20);
		assert_eq!(<Test as Config>::MinEligibleCollators::get(), 1);
		assert_eq!(<Test as Config>::KickThreshold::get(), 10);
		assert_eq!(<Test as Config>::MaxStakedCandidates::get(), 16);
		assert_eq!(<Test as Config>::CollatorUnstakingDelay::get(), 5);
		assert_eq!(<Test as Config>::UserUnstakingDelay::get(), 2);
		// should always be MaxInvulnerables + MaxCandidates
		assert_eq!(MaxDesiredCandidates::<Test>::get(), 40);

		assert_eq!(DesiredCandidates::<Test>::get(), 2);
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert_eq!(MinStake::<Test>::get(), 2);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(CollatorRewardPercentage::<Test>::get(), Percent::from_parts(20));
		// The minimum balance should not have been minted
		assert_eq!(Balances::free_balance(CollatorStaking::account_id()), 0);
		// genesis should sort input
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		#[cfg(feature = "try-runtime")]
		{
			use frame_system::pallet_prelude::BlockNumberFor;
			assert_ok!(
				<CollatorStaking as frame_support::traits::Hooks<BlockNumberFor<Test>>>::try_state(
					1
				)
			);
		}
	});
}

#[test]
fn it_should_set_invulnerables() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		let new_set = vec![1, 4, 3, 2];
		assert_ok!(CollatorStaking::set_invulnerables(
			RuntimeOrigin::signed(RootAccount::get()),
			new_set.clone()
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewInvulnerables {
			invulnerables: vec![1, 2, 3, 4],
		}));
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3, 4]);

		// cannot set with non-root.
		assert_noop!(
			CollatorStaking::set_invulnerables(RuntimeOrigin::signed(1), new_set),
			BadOrigin
		);
	});
}

#[test]
fn cannot_empty_invulnerables_if_not_enough_candidates() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			CollatorStaking::set_invulnerables(RuntimeOrigin::signed(RootAccount::get()), vec![]),
			Error::<Test>::TooFewEligibleCollators
		);
	});
}

#[test]
fn it_should_set_invulnerables_even_with_some_invalid() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
		let new_with_invalid = vec![1, 4, 3, 42, 2, 1000];

		assert_ok!(CollatorStaking::set_invulnerables(
			RuntimeOrigin::signed(RootAccount::get()),
			new_with_invalid
		));
		System::assert_has_event(RuntimeEvent::CollatorStaking(
			Event::InvalidInvulnerableSkipped { account_id: 42 },
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewInvulnerables {
			invulnerables: vec![1, 2, 3, 4],
		}));

		// should succeed and order them, but not include 42
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3, 4]);
	});
}

#[test]
fn add_invulnerable_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
		let new = 3;

		// function runs
		assert_ok!(CollatorStaking::add_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			new
		));

		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
			account_id: new,
		}));

		// same element cannot be added more than once
		assert_noop!(
			CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), new),
			Error::<Test>::AlreadyInvulnerable
		);

		// new element is now part of the invulnerables list
		assert!(Invulnerables::<Test>::get().to_vec().contains(&new));

		// cannot add with non-root
		assert_noop!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(1), new), BadOrigin);

		// cannot add invulnerable without associated validator keys
		let not_validator = 42;
		assert_noop!(
			CollatorStaking::add_invulnerable(
				RuntimeOrigin::signed(RootAccount::get()),
				not_validator
			),
			Error::<Test>::CollatorNotRegistered
		);
	});
}

#[test]
fn invulnerable_limit_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		// MaxInvulnerables: u32 = 20
		for ii in 3..=21 {
			// only keys were registered in mock for 1 to 5
			if ii > 5 {
				Balances::make_free_balance_be(&ii, 100);
				let key = MockSessionKeys { aura: UintAuthorityId(ii) };
				Session::set_keys(RuntimeOrigin::signed(ii).into(), key, Vec::new()).unwrap();
			}
			assert_eq!(Balances::free_balance(ii), 100);
			if ii < 21 {
				assert_ok!(CollatorStaking::add_invulnerable(
					RuntimeOrigin::signed(RootAccount::get()),
					ii
				));
				System::assert_last_event(RuntimeEvent::CollatorStaking(
					Event::InvulnerableAdded { account_id: ii },
				));
			} else {
				assert_noop!(
					CollatorStaking::add_invulnerable(
						RuntimeOrigin::signed(RootAccount::get()),
						ii
					),
					Error::<Test>::TooManyInvulnerables
				);
			}
		}
		let expected: Vec<u64> = (1..=20).collect();
		assert_eq!(Invulnerables::<Test>::get(), expected);

		// Cannot set too many Invulnerables
		let too_many_invulnerables: Vec<u64> = (1..=21).collect();
		assert_noop!(
			CollatorStaking::set_invulnerables(
				RuntimeOrigin::signed(RootAccount::get()),
				too_many_invulnerables
			),
			Error::<Test>::TooManyInvulnerables
		);
		assert_eq!(Invulnerables::<Test>::get(), expected);
	});
}

#[test]
fn remove_invulnerable_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		assert_ok!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 4));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
			account_id: 4,
		}));
		assert_ok!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 3));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
			account_id: 3,
		}));

		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3, 4]);

		assert_ok!(CollatorStaking::remove_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			2
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableRemoved {
			account_id: 2,
		}));
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 3, 4]);

		// cannot remove invulnerable not in the list
		assert_noop!(
			CollatorStaking::remove_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 2),
			Error::<Test>::NotInvulnerable
		);

		// cannot remove without privilege
		assert_noop!(CollatorStaking::remove_invulnerable(RuntimeOrigin::signed(1), 3), BadOrigin);
	});
}

#[test]
fn candidate_to_invulnerable_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(DesiredCandidates::<Test>::get(), 2);
		assert_eq!(CandidacyBond::<Test>::get(), 10);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(Balances::free_balance(4), 100);

		register_candidates(3..=4);

		assert_eq!(Stake::<Test>::get(3, 3), 10);
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(4, 4), 10);
		assert_eq!(Balances::free_balance(4), 90);

		assert_ok!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 3));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::CandidateRemoved {
			account_id: 3,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
			account_id: 3,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 3,
			candidate: 3,
			amount: 10,
		}));
		assert!(Invulnerables::<Test>::get().to_vec().contains(&3));
		assert_eq!(Stake::<Test>::get(3, 3), 0);
		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 1);

		assert_ok!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 4));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::CandidateRemoved {
			account_id: 4,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
			account_id: 4,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 4,
			candidate: 4,
			amount: 10,
		}));
		assert!(Invulnerables::<Test>::get().to_vec().contains(&4));
		assert_eq!(Stake::<Test>::get(4, 4), 0);
		assert_eq!(Balances::free_balance(4), 100);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
	});
}

#[test]
fn set_desired_candidates_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		// given
		assert_eq!(DesiredCandidates::<Test>::get(), 2);

		// can set
		assert_ok!(CollatorStaking::set_desired_candidates(
			RuntimeOrigin::signed(RootAccount::get()),
			40
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewDesiredCandidates {
			desired_candidates: 40,
		}));
		assert_eq!(DesiredCandidates::<Test>::get(), 40);

		// rejects bad origin
		assert_noop!(
			CollatorStaking::set_desired_candidates(RuntimeOrigin::signed(1), 8),
			BadOrigin
		);
		// rejects bad origin
		assert_noop!(
			CollatorStaking::set_desired_candidates(RuntimeOrigin::signed(RootAccount::get()), 50),
			Error::<Test>::TooManyDesiredCandidates
		);
	});
}

#[test]
fn set_candidacy_bond_empty_candidate_list() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		// given
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert!(CandidateList::<Test>::get().is_empty());

		// can decrease without candidates
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			7
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 7,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 7);
		assert!(CandidateList::<Test>::get().is_empty());

		// rejects bad origin.
		assert_noop!(CollatorStaking::set_candidacy_bond(RuntimeOrigin::signed(1), 8), BadOrigin);

		// can increase without candidates
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			20
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 20,
		}));
		assert!(CandidateList::<Test>::get().is_empty());
		assert_eq!(CandidacyBond::<Test>::get(), 20);
	});
}

#[test]
fn set_candidacy_bond_with_one_candidate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// given
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert!(CandidateList::<Test>::get().is_empty());

		let candidate_3 = CandidateInfo { who: 3, deposit: 10, stakers: 1 };

		register_candidates(3..=3);
		assert_eq!(CandidateList::<Test>::get(), vec![candidate_3.clone()]);
		assert_eq!(Stake::<Test>::get(3, 3), 10);

		// can decrease with one candidate
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			7
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 7,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 7);
		initialize_to_block(10);
		assert_eq!(CandidateList::<Test>::get(), vec![candidate_3.clone()]);

		// can increase up to initial deposit
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			10
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 10,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		initialize_to_block(20);
		assert_eq!(CandidateList::<Test>::get(), vec![candidate_3.clone()]);

		// can increase past initial deposit, kicking candidates under the new value
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			20
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 20,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 20);
		initialize_to_block(30);
		assert_eq!(CandidateList::<Test>::get(), vec![]);
	});
}

#[test]
fn set_candidacy_bond_with_many_candidates_same_deposit() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		// given
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert!(CandidateList::<Test>::get().is_empty());

		let candidate_3 = CandidateInfo { who: 3, deposit: 10, stakers: 1 };
		let candidate_4 = CandidateInfo { who: 4, deposit: 10, stakers: 1 };
		let candidate_5 = CandidateInfo { who: 5, deposit: 10, stakers: 1 };

		register_candidates(3..=5);

		assert_eq!(
			CandidateList::<Test>::get(),
			vec![candidate_5.clone(), candidate_4.clone(), candidate_3.clone()]
		);

		// can decrease with multiple candidates
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			2
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 2,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 2);
		CollatorStaking::kick_stale_candidates();
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![candidate_5.clone(), candidate_4.clone(), candidate_3.clone()]
		);

		// can increase up to initial deposit
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			10
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 10,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		CollatorStaking::kick_stale_candidates();
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![candidate_5.clone(), candidate_4.clone(), candidate_3.clone()]
		);

		// can increase past initial deposit
		assert_ok!(CollatorStaking::set_candidacy_bond(
			RuntimeOrigin::signed(RootAccount::get()),
			20
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewCandidacyBond {
			bond_amount: 20,
		}));
		assert_eq!(CandidacyBond::<Test>::get(), 20);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 20));
		let new_candidate_5 = CandidateInfo { who: 5, deposit: 30, stakers: 1 };
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![candidate_4.clone(), candidate_3.clone(), new_candidate_5.clone()]
		);
		CollatorStaking::kick_stale_candidates();
		assert_eq!(CandidateList::<Test>::get(), vec![new_candidate_5]);
	});
}

#[test]
fn cannot_set_candidacy_bond_lower_than_min_stake() {
	new_test_ext().execute_with(|| {
		// given
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert_eq!(MinStake::<Test>::get(), 2);

		// then
		assert_noop!(
			CollatorStaking::set_candidacy_bond(RuntimeOrigin::signed(RootAccount::get()), 1),
			Error::<Test>::InvalidCandidacyBond
		);
	});
}

#[test]
fn cannot_register_candidate_if_too_many() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		DesiredCandidates::<Test>::put(1);

		// MaxCandidates: u32 = 20
		// Aside from 3, 4, and 5, create enough accounts to have 21 potential
		// candidates.
		for acc in 6..=23 {
			fund_account(acc);
			register_keys(acc);
		}
		register_candidates(3..=22);

		assert_noop!(
			CollatorStaking::register_as_candidate(RuntimeOrigin::signed(23)),
			Error::<Test>::TooManyCandidates,
		);
	})
}

#[test]
fn cannot_unregister_candidate_if_too_few() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
		assert_ok!(CollatorStaking::remove_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			1
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableRemoved {
			account_id: 1,
		}));
		assert_noop!(
			CollatorStaking::remove_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 2),
			Error::<Test>::TooFewEligibleCollators,
		);

		// reset desired candidates:
		DesiredCandidates::<Test>::put(1);
		register_candidates(4..=4);

		// now we can remove `2`
		assert_ok!(CollatorStaking::remove_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			2
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableRemoved {
			account_id: 2,
		}));

		// can not remove too few
		assert_noop!(
			CollatorStaking::leave_intent(RuntimeOrigin::signed(4)),
			Error::<Test>::TooFewEligibleCollators,
		);
	})
}

#[test]
fn cannot_register_as_candidate_if_invulnerable() {
	new_test_ext().execute_with(|| {
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		// can't 1 because it is invulnerable.
		assert_noop!(
			CollatorStaking::register_as_candidate(RuntimeOrigin::signed(1)),
			Error::<Test>::AlreadyInvulnerable,
		);
	})
}

#[test]
fn cannot_register_as_candidate_if_keys_not_registered() {
	new_test_ext().execute_with(|| {
		// can't 42 because keys not registered.
		assert_noop!(
			CollatorStaking::register_as_candidate(RuntimeOrigin::signed(42)),
			Error::<Test>::CollatorNotRegistered
		);
	})
}

#[test]
fn cannot_register_dupe_candidate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// can add 3 as candidate
		register_candidates(3..=3);
		let addition = CandidateInfo { who: 3, deposit: 10, stakers: 1 };
		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![addition]
		);
		assert_eq!(LastAuthoredBlock::<Test>::get(3), 11);
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(3, 3), 10);

		// but no more
		assert_noop!(
			CollatorStaking::register_as_candidate(RuntimeOrigin::signed(3)),
			Error::<Test>::AlreadyCandidate,
		);
	})
}

#[test]
fn cannot_register_as_candidate_if_poor() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);
		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(Balances::free_balance(33), 0);

		// works
		register_candidates(3..=3);

		// poor
		assert_noop!(
			CollatorStaking::register_as_candidate(RuntimeOrigin::signed(33)),
			BalancesError::<Test>::InsufficientBalance,
		);
	});
}

#[test]
fn register_as_candidate_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// given
		assert_eq!(DesiredCandidates::<Test>::get(), 2);
		assert_eq!(CandidacyBond::<Test>::get(), 10);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		// take two endowed, non-invulnerables accounts.
		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(Stake::<Test>::get(3, 3), 0);
		assert_eq!(Balances::free_balance(4), 100);
		assert_eq!(Stake::<Test>::get(4, 4), 0);

		register_candidates(3..=4);

		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(3, 3), 10);
		assert_eq!(Balances::free_balance(4), 90);
		assert_eq!(Stake::<Test>::get(4, 4), 10);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 2);
	});
}

#[test]
fn cannot_take_candidate_slot_if_invulnerable() {
	new_test_ext().execute_with(|| {
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		// can't 1 because it is invulnerable.
		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(1), 50u64.into(), 2),
			Error::<Test>::AlreadyInvulnerable,
		);
	})
}

#[test]
fn cannot_take_candidate_slot_if_list_not_full() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=21);
		assert_eq!(CandidateList::<Test>::decode_len().unwrap_or_default(), 19);
		assert_eq!(<Test as Config>::MaxCandidates::get(), 20);

		fund_account(22);
		register_keys(22);
		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(22), 50u64.into(), 3),
			Error::<Test>::CanRegister,
		);
	})
}

#[test]
fn cannot_take_candidate_slot_if_keys_not_registered() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(3)));
		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(42), 50u64.into(), 3),
			Error::<Test>::CollatorNotRegistered
		);
	})
}

#[test]
fn cannot_take_candidate_slot_if_duplicate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// we cannot take a candidate slot if the list is not already full
		register_candidates(3..=22);

		let actual_candidates = CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>();
		assert_eq!(actual_candidates.len(), 20);
		assert_eq!(LastAuthoredBlock::<Test>::get(3), 11);
		assert_eq!(LastAuthoredBlock::<Test>::get(4), 11);
		assert_eq!(Balances::free_balance(3), 90);

		// but no more
		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(3), 50u64.into(), 4),
			Error::<Test>::AlreadyCandidate,
		);
	})
}

#[test]
fn cannot_take_candidate_slot_if_target_invalid() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(4..=23);
		assert_eq!(CandidateList::<Test>::get().len(), 20);

		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(3), 11u64.into(), 24),
			Error::<Test>::NotCandidate,
		);
	})
}

#[test]
fn cannot_take_candidate_slot_if_poor() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(4..=23);
		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(Balances::free_balance(33), 0);

		// works
		assert_ok!(CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(3), 20u64.into(), 4));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::CandidateReplaced {
			old: 4,
			new: 3,
			deposit: 20,
		}));

		// poor
		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(33), 30u64.into(), 3),
			BalancesError::<Test>::InsufficientBalance,
		);
	});
}

#[test]
fn cannot_take_candidate_slot_if_insufficient_deposit() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=3);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 60u64.into()));
		assert_eq!(Balances::free_balance(3), 30);
		assert_eq!(Stake::<Test>::get(3, 3), 70);
		assert_eq!(Balances::free_balance(4), 100);
		assert_eq!(Stake::<Test>::get(4, 4), 0);

		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(4), 5u64.into(), 3),
			Error::<Test>::InsufficientBond,
		);

		assert_eq!(Balances::free_balance(3), 30);
		assert_eq!(Stake::<Test>::get(3, 3), 70);
		assert_eq!(Balances::free_balance(4), 100);
		assert_eq!(Stake::<Test>::get(4, 4), 0);
	});
}

#[test]
fn cannot_take_candidate_slot_if_deposit_less_than_target() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		fund_account(23);
		register_keys(23);

		register_candidates(3..=22);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 60u64.into()));

		assert_eq!(Balances::free_balance(3), 30);
		assert_eq!(Stake::<Test>::get(3, 3), 70);
		assert_eq!(Balances::free_balance(23), 100);
		assert_eq!(Stake::<Test>::get(23, 23), 0);

		assert_noop!(
			CollatorStaking::take_candidate_slot(RuntimeOrigin::signed(23), 20u64.into(), 3),
			Error::<Test>::InsufficientBond,
		);

		assert_eq!(Balances::free_balance(3), 30);
		assert_eq!(Stake::<Test>::get(3, 3), 70);
		assert_eq!(Balances::free_balance(23), 100);
		assert_eq!(Stake::<Test>::get(23, 23), 0);
	});
}

#[test]
fn take_candidate_slot_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// given
		assert_eq!(DesiredCandidates::<Test>::get(), 2);
		assert_eq!(CandidacyBond::<Test>::get(), 10);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		register_candidates(3..=22);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 20);

		fund_account(23);
		register_keys(23);

		assert_ok!(CollatorStaking::take_candidate_slot(
			RuntimeOrigin::signed(23),
			50u64.into(),
			4
		));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeAdded {
			staker: 23,
			candidate: 23,
			amount: 10, // candidacy bond
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeAdded {
			staker: 23,
			candidate: 23,
			amount: 40, // rest of the stake
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 4,
			candidate: 4,
			amount: 10,
		}));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::CandidateReplaced {
			old: 4,
			new: 23,
			deposit: 50,
		}));

		assert_eq!(UnstakingRequests::<Test>::get(4), vec![]);
		assert_eq!(Balances::free_balance(4), 100);
		assert_eq!(Stake::<Test>::get(4, 4), 0);
		assert_eq!(Balances::free_balance(23), 50);
		assert_eq!(Stake::<Test>::get(23, 23), 50);
	});
}

#[test]
fn candidate_list_works() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// given
		assert_eq!(DesiredCandidates::<Test>::get(), 2);
		assert_eq!(CandidacyBond::<Test>::get(), 10);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);

		// take three endowed, non-invulnerables accounts.
		assert_eq!(Balances::free_balance(3), 100);
		assert_eq!(Stake::<Test>::get(3, 3), 0);
		assert_eq!(Balances::free_balance(4), 100);
		assert_eq!(Stake::<Test>::get(4, 4), 0);
		assert_eq!(Balances::free_balance(5), 100);
		assert_eq!(Stake::<Test>::get(5, 5), 0);
		register_candidates(3..=5);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 20));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 30));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 4, 25));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 30));

		let candidate_3 = CandidateInfo { who: 3, deposit: 40, stakers: 1 };
		let candidate_4 = CandidateInfo { who: 4, deposit: 35, stakers: 1 };
		let candidate_5 = CandidateInfo { who: 5, deposit: 60, stakers: 1 };
		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![candidate_4, candidate_3, candidate_5]
		);
	});
}

#[test]
fn leave_intent() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		// register a candidate.
		register_candidates(3..=3);
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(3, 3), 10);

		// register too so can leave above min candidates
		register_candidates(5..=5);
		assert_eq!(Balances::free_balance(5), 90);
		assert_eq!(Stake::<Test>::get(5, 5), 10);

		// cannot leave if not candidate.
		assert_noop!(
			CollatorStaking::leave_intent(RuntimeOrigin::signed(4)),
			Error::<Test>::NotCandidate
		);

		// Unstake request is created
		assert_eq!(UnstakingRequests::<Test>::get(3), vec![]);
		assert_ok!(CollatorStaking::leave_intent(RuntimeOrigin::signed(3)));

		let unstake_request = UnstakeRequest { block: 6, amount: 10 };
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(3, 3), 0);
		assert_eq!(UnstakingRequests::<Test>::get(3), vec![unstake_request]);
		assert_eq!(LastAuthoredBlock::<Test>::get(3), 0);
	});
}

#[test]
fn fees_edgecases() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		Balances::make_free_balance_be(&CollatorStaking::account_id(), Balances::minimum_balance());

		// Nothing panics, no reward when no ED in balance
		Authorship::on_initialize(1);
		// 4 is the default author.
		assert_eq!(Balances::free_balance(4), 100);
		register_candidates(4..=4);
		// triggers `note_author`
		Authorship::on_initialize(1);

		// tuple of (id, deposit).
		let collator = CandidateInfo { who: 4, deposit: 10, stakers: 1 };

		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![collator]
		);
		assert_eq!(LastAuthoredBlock::<Test>::get(4), 1);
		// Nothing received
		assert_eq!(Balances::free_balance(4), 90);
		// all fee stays
		assert_eq!(Balances::free_balance(CollatorStaking::account_id()), 5);
	});
}

#[test]
fn session_management_single_candidate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		// add a new collator
		register_candidates(3..=3);

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 1);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 3.
		assert_eq!(Session::queued_keys().len(), 3);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 3]);
	});
}

#[test]
fn session_management_max_candidates() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		register_candidates(3..=5);

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 3);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 4.
		assert_eq!(Session::queued_keys().len(), 4);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 3, 4]);
	});
}

#[test]
fn session_management_increase_bid_with_list_update() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		register_candidates(3..=5);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 60));

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 3);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 4.
		assert_eq!(Session::queued_keys().len(), 4);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 5, 3]);
	});
}

#[test]
fn session_management_candidate_list_eager_sort() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		register_candidates(3..=5);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 60));

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 3);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 4.
		assert_eq!(Session::queued_keys().len(), 4);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 5, 3]);
	});
}

#[test]
fn session_management_reciprocal_outbidding() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		register_candidates(3..=5);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 60));

		initialize_to_block(5);

		// candidates 3 and 4 saw they were outbid and preemptively bid more
		// than 5 in the next block.
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 4, 70));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 70));

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 3);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 4.
		assert_eq!(Session::queued_keys().len(), 4);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 4, 3]);
	});
}

#[test]
fn session_management_decrease_bid_after_auction() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(4);

		assert_eq!(SessionChangeBlock::get(), 0);
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		register_candidates(3..=5);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 60));

		initialize_to_block(5);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 4, 70));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 70));

		initialize_to_block(5);

		// candidate 5 saw it was outbid and wants to take back its bid, but
		// not entirely so, they still keep their place in the candidate list
		// in case there is an opportunity in the future.
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 10));

		// session won't see this.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);
		// but we have a new candidate.
		assert_eq!(CandidateList::<Test>::get().iter().count(), 3);

		initialize_to_block(10);
		assert_eq!(SessionChangeBlock::get(), 10);
		// pallet-session has 1 session delay; current validators are the same.
		assert_eq!(Session::validators(), vec![1, 2]);
		// queued ones are changed, and now we have 4.
		assert_eq!(Session::queued_keys().len(), 4);
		// session handlers (aura, et. al.) cannot see this yet.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2]);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// changed are now reflected to session handlers.
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 4, 3]);
	});
}

#[test]
fn kick_mechanism() {
	new_test_ext().execute_with(|| {
		// add a new collator
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(3)));
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(4)));
		initialize_to_block(10);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 2);
		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// 4 authored this block, gets to stay 3 was kicked
		assert_eq!(CandidateList::<Test>::get().iter().count(), 1);
		// 3 will be kicked after 1 session delay
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 3, 4]);
		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![CandidateInfo { who: 4, deposit: 10, stakers: 1 }]
		);
		assert_eq!(LastAuthoredBlock::<Test>::get(4), 20);
		initialize_to_block(30);
		// 3 gets kicked after 1 session delay
		assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 4]);
		// kicked collator gets funds back after a delay
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(
			UnstakingRequests::<Test>::get(3),
			vec![UnstakeRequest { block: 25, amount: 10 }]
		);
	});
}

#[test]
fn should_not_kick_mechanism_too_few() {
	new_test_ext().execute_with(|| {
		// remove the invulnerables and add new collators 3 and 5

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
		assert_ok!(CollatorStaking::remove_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			1
		));
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(3)));
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(5)));
		assert_ok!(CollatorStaking::remove_invulnerable(
			RuntimeOrigin::signed(RootAccount::get()),
			2
		));

		initialize_to_block(10);
		assert_eq!(CandidateList::<Test>::get().iter().count(), 2);

		initialize_to_block(20);
		assert_eq!(SessionChangeBlock::get(), 20);
		// 4 authored this block, 3 is kicked, 5 stays because of too few collators
		assert_eq!(CandidateList::<Test>::get().iter().count(), 1);
		// 3 will be kicked after 1 session delay
		assert_eq!(SessionHandlerCollators::get(), vec![3, 5]);
		// tuple of (id, deposit).
		let collator = CandidateInfo { who: 3, deposit: 10, stakers: 1 };
		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![collator]
		);
		assert_eq!(LastAuthoredBlock::<Test>::get(4), 20);

		initialize_to_block(30);
		// 3 gets kicked after 1 session delay
		assert_eq!(SessionHandlerCollators::get(), vec![3]);
		// kicked collator gets funds back after a delay
		assert_eq!(Balances::free_balance(5), 90);
		assert_eq!(
			UnstakingRequests::<Test>::get(5),
			vec![UnstakeRequest { block: 25, amount: 10 }]
		);
	});
}

#[test]
fn should_kick_invulnerables_from_candidates_on_session_change() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
		register_candidates(3..=4);
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Balances::free_balance(4), 90);
		assert_ok!(CollatorStaking::set_invulnerables(
			RuntimeOrigin::signed(RootAccount::get()),
			vec![1, 2, 3]
		));

		// tuple of (id, deposit).
		let collator_3 = CandidateInfo { who: 3, deposit: 10, stakers: 1 };
		let collator_4 = CandidateInfo { who: 4, deposit: 10, stakers: 1 };

		let actual_candidates = CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>();
		assert_eq!(actual_candidates, vec![collator_4.clone(), collator_3]);
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3]);

		// session change
		initialize_to_block(10);
		// 3 is removed from candidates
		assert_eq!(
			CandidateList::<Test>::get().iter().cloned().collect::<Vec<_>>(),
			vec![collator_4]
		);
		// but not from invulnerables
		assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3]);
		// and it got its deposit back
		assert_eq!(Balances::free_balance(3), 100);
	});
}

#[test]
#[should_panic = "duplicate invulnerables in genesis."]
fn cannot_set_genesis_value_twice() {
	sp_tracing::try_init_simple();
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
	let invulnerables = vec![1, 1];

	let collator_staking = collator_staking::GenesisConfig::<Test> {
		desired_candidates: 2,
		candidacy_bond: 10,
		min_stake: 1,
		invulnerables,
		collator_reward_percentage: Percent::from_parts(20),
		extra_reward: 0,
	};
	// collator selection must be initialized before session.
	collator_staking.assimilate_storage(&mut t).unwrap();
}

#[test]
#[should_panic = "min_stake is higher than candidacy_bond"]
fn cannot_set_invalid_min_stake_in_genesis() {
	sp_tracing::try_init_simple();
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();

	let collator_staking = collator_staking::GenesisConfig::<Test> {
		desired_candidates: 2,
		candidacy_bond: 10,
		min_stake: 15,
		invulnerables: vec![1, 2],
		collator_reward_percentage: Percent::from_parts(20),
		extra_reward: 0,
	};
	// collator selection must be initialized before session.
	collator_staking.assimilate_storage(&mut t).unwrap();
}

#[test]
#[should_panic = "genesis desired_candidates are more than T::MaxCandidates"]
fn cannot_set_invalid_max_candidates_in_genesis() {
	sp_tracing::try_init_simple();
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();

	let collator_staking = collator_staking::GenesisConfig::<Test> {
		desired_candidates: 50,
		candidacy_bond: 10,
		min_stake: 2,
		invulnerables: vec![1, 2],
		collator_reward_percentage: Percent::from_parts(20),
		extra_reward: 0,
	};
	// collator selection must be initialized before session.
	collator_staking.assimilate_storage(&mut t).unwrap();
}

#[test]
#[should_panic = "genesis invulnerables are more than T::MaxInvulnerables"]
fn cannot_set_too_many_invulnerables_at_genesis() {
	sp_tracing::try_init_simple();
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();

	let collator_staking = collator_staking::GenesisConfig::<Test> {
		desired_candidates: 5,
		candidacy_bond: 10,
		min_stake: 2,
		invulnerables: vec![
			1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
		],
		collator_reward_percentage: Percent::from_parts(20),
		extra_reward: 0,
	};
	// collator selection must be initialized before session.
	collator_staking.assimilate_storage(&mut t).unwrap();
}

#[test]
fn cannot_stake_if_not_candidate() {
	new_test_ext().execute_with(|| {
		// invulnerable
		assert_noop!(
			CollatorStaking::stake(RuntimeOrigin::signed(4), 1, 1),
			Error::<Test>::NotCandidate
		);
		// not registered as candidate
		assert_noop!(
			CollatorStaking::stake(RuntimeOrigin::signed(4), 5, 15),
			Error::<Test>::NotCandidate
		);
	});
}

#[test]
fn cannot_stake_if_under_minstake() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=3);
		assert_noop!(
			CollatorStaking::stake(RuntimeOrigin::signed(4), 3, 1),
			Error::<Test>::InsufficientStake
		);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 3, 2));
		assert_eq!(Balances::free_balance(4), 98);
		assert_eq!(Stake::<Test>::get(3, 4), 2);

		// After adding MinStake it should work
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 3, 1));
		assert_eq!(Balances::free_balance(4), 97);
		assert_eq!(Stake::<Test>::get(3, 4), 3);
	});
}

#[test]
fn stake() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=3);
		assert_eq!(Balances::free_balance(3), 90);
		assert_eq!(Stake::<Test>::get(3, 3), 10);
		assert_eq!(StakeCount::<Test>::get(3), 1);
		assert_eq!(CandidateList::<Test>::get()[0].deposit, 10);

		assert_eq!(StakeCount::<Test>::get(4), 0);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 3, 2));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeAdded {
			staker: 4,
			candidate: 3,
			amount: 2,
		}));
		assert_eq!(Balances::free_balance(4), 98);
		assert_eq!(Stake::<Test>::get(3, 4), 2);
		assert_eq!(Stake::<Test>::get(3, 3), 10);
		assert_eq!(CandidateList::<Test>::get()[0].deposit, 12);
		assert_eq!(StakeCount::<Test>::get(4), 1);
	});
}

#[test]
fn stake_and_reassign_position() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=4);
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 10, stakers: 1 },
				CandidateInfo { who: 3, deposit: 10, stakers: 1 },
			]
		);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 2));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 10, stakers: 1 },
				CandidateInfo { who: 3, deposit: 12, stakers: 2 },
			]
		);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 4, 5));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 12, stakers: 2 },
				CandidateInfo { who: 4, deposit: 15, stakers: 2 },
			]
		);

		register_candidates(5..=5);
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 5, deposit: 10, stakers: 1 },
				CandidateInfo { who: 3, deposit: 12, stakers: 2 },
				CandidateInfo { who: 4, deposit: 15, stakers: 2 },
			]
		);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 3));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 12, stakers: 2 },
				CandidateInfo { who: 5, deposit: 13, stakers: 1 },
				CandidateInfo { who: 4, deposit: 15, stakers: 2 },
			]
		);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 7));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 12, stakers: 2 },
				CandidateInfo { who: 4, deposit: 15, stakers: 2 },
				CandidateInfo { who: 5, deposit: 20, stakers: 1 },
			]
		);
	});
}

#[test]
fn cannot_stake_too_many_candidates() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(<Test as Config>::MaxStakedCandidates::get(), 16);

		register_candidates(3..=19);
		for i in 3..=18 {
			assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(1), i, 2));
		}
		assert_eq!(StakeCount::<Test>::get(1), 16);
		assert_noop!(
			CollatorStaking::stake(RuntimeOrigin::signed(1), 19, 2),
			Error::<Test>::TooManyStakedCandidates
		);
	});
}

#[test]
fn cannot_stake_invulnerable() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			CollatorStaking::stake(RuntimeOrigin::signed(3), 1, 2),
			Error::<Test>::NotCandidate
		);
	});
}

#[test]
fn unstake_from_candidate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=4);
		assert_eq!(StakeCount::<Test>::get(5), 0);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 20));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 4, 10));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 20, stakers: 2 },
				CandidateInfo { who: 3, deposit: 30, stakers: 2 },
			]
		);

		// unstake from actual candidate
		assert_eq!(Balances::free_balance(5), 70);
		assert_eq!(StakeCount::<Test>::get(5), 2);
		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(5), 3));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 5,
			candidate: 3,
			amount: 20,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::UnstakeRequestCreated {
			staker: 5,
			amount: 20,
			block: 3,
		}));
		// candidate list gets reordered
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 10, stakers: 1 },
				CandidateInfo { who: 4, deposit: 20, stakers: 2 },
			]
		);
		assert_eq!(StakeCount::<Test>::get(5), 1);
		assert_eq!(Stake::<Test>::get(3, 5), 0);
		assert_eq!(Stake::<Test>::get(4, 5), 10);
		assert_eq!(Balances::free_balance(5), 70);
		assert_eq!(
			UnstakingRequests::<Test>::get(5),
			vec![UnstakeRequest { block: 3, amount: 20 }]
		);
	});
}

#[test]
fn unstake_self() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(StakeCount::<Test>::get(3), 0);
		register_candidates(3..=4);
		assert_eq!(StakeCount::<Test>::get(3), 1);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 20));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 4, 10));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 20, stakers: 2 },
				CandidateInfo { who: 3, deposit: 30, stakers: 1 },
			]
		);

		// unstake from actual candidate
		assert_eq!(Balances::free_balance(3), 60);
		assert_eq!(StakeCount::<Test>::get(3), 2);
		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(3), 3));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 3,
			candidate: 3,
			amount: 30,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::UnstakeRequestCreated {
			staker: 3,
			amount: 30,
			block: 6, // higher delay
		}));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 0, stakers: 0 },
				CandidateInfo { who: 4, deposit: 20, stakers: 2 }
			]
		);
		assert_eq!(StakeCount::<Test>::get(3), 1);
		assert_eq!(Stake::<Test>::get(3, 3), 0);
		assert_eq!(Stake::<Test>::get(4, 3), 10);
		assert_eq!(Balances::free_balance(3), 60);
		assert_eq!(
			UnstakingRequests::<Test>::get(3),
			vec![UnstakeRequest { block: 6, amount: 30 }]
		);

		// check after unstaking with a shorter delay the list remains sorted by block
		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(3), 4));
		assert_eq!(
			UnstakingRequests::<Test>::get(3),
			vec![UnstakeRequest { block: 3, amount: 10 }, UnstakeRequest { block: 6, amount: 30 }]
		);
	});
}

#[test]
fn unstake_from_ex_candidate() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=4);
		assert_eq!(StakeCount::<Test>::get(5), 0);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 20));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 4, 10));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 20, stakers: 2 },
				CandidateInfo { who: 3, deposit: 30, stakers: 2 },
			]
		);
		assert_eq!(Stake::<Test>::get(3, 5), 20);
		assert_eq!(Stake::<Test>::get(4, 5), 10);

		// unstake from ex-candidate
		assert_eq!(StakeCount::<Test>::get(5), 2);
		assert_ok!(CollatorStaking::leave_intent(RuntimeOrigin::signed(3)));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![CandidateInfo { who: 4, deposit: 20, stakers: 2 }]
		);

		assert_eq!(StakeCount::<Test>::get(5), 2);
		assert_eq!(Balances::free_balance(5), 70);
		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(5), 3));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 5,
			candidate: 3,
			amount: 20,
		}));
		assert_eq!(UnstakingRequests::<Test>::get(5), vec![]);
		assert_eq!(Stake::<Test>::get(3, 5), 0);
		assert_eq!(Stake::<Test>::get(4, 5), 10);
		assert_eq!(StakeCount::<Test>::get(5), 1);
		assert_eq!(Balances::free_balance(5), 90);
	});
}

#[test]
fn unstake_fails_if_over_limit() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(<Test as Config>::MaxStakedCandidates::get(), 16);
		register_candidates(3..=18);

		for pos in 3..=18 {
			assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), pos, 2));
		}
		// now we accumulate 16 requests, the maximum.
		assert_ok!(CollatorStaking::unstake_all(RuntimeOrigin::signed(5)));
		assert_eq!(UnstakingRequests::<Test>::get(5).len(), 16);

		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 2));
		assert_noop!(
			CollatorStaking::unstake_from(RuntimeOrigin::signed(5), 3),
			Error::<Test>::TooManyUnstakingRequests
		);

		// if we claim the requests we can keep unstaking.
		initialize_to_block(3);
		assert_ok!(CollatorStaking::claim(RuntimeOrigin::signed(5)));
		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(5), 3));
	});
}

#[test]
fn unstake_all() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=4);
		assert_eq!(StakeCount::<Test>::get(5), 0);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 20));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 4, 10));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 20, stakers: 2 },
				CandidateInfo { who: 3, deposit: 30, stakers: 2 },
			]
		);

		assert_eq!(StakeCount::<Test>::get(5), 2);
		assert_ok!(CollatorStaking::leave_intent(RuntimeOrigin::signed(3)));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![CandidateInfo { who: 4, deposit: 20, stakers: 2 }]
		);

		assert_eq!(StakeCount::<Test>::get(5), 2);
		assert_eq!(Balances::free_balance(5), 70);
		assert_ok!(CollatorStaking::unstake_all(RuntimeOrigin::signed(5)));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 5,
			candidate: 3,
			amount: 20,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 5,
			candidate: 4,
			amount: 10,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::UnstakeRequestCreated {
			staker: 5,
			amount: 10,
			block: 3,
		}));
		assert_eq!(
			UnstakingRequests::<Test>::get(5),
			vec![UnstakeRequest { block: 3, amount: 10 }]
		);
		assert_eq!(Stake::<Test>::get(3, 5), 0);
		assert_eq!(Stake::<Test>::get(4, 5), 0);
		assert_eq!(StakeCount::<Test>::get(5), 0);
		assert_eq!(Balances::free_balance(5), 90);
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![CandidateInfo { who: 4, deposit: 10, stakers: 1 }]
		);
	});
}

#[test]
fn claim_with_empty_list() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(System::events(), vec![]);
		assert_eq!(UnstakingRequests::<Test>::get(5), vec![]);
		assert_ok!(CollatorStaking::claim(RuntimeOrigin::signed(5)));
		assert_eq!(System::events(), vec![]);
		assert_eq!(UnstakingRequests::<Test>::get(5), vec![]);
	});
}

#[test]
fn claim() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		register_candidates(3..=3);
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 20));
		assert_eq!(Stake::<Test>::get(3, 5), 20);
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![CandidateInfo { who: 3, deposit: 30, stakers: 2 }]
		);

		assert_ok!(CollatorStaking::unstake_from(RuntimeOrigin::signed(5), 3));
		assert_eq!(StakeCount::<Test>::get(5), 0);
		assert_eq!(Balances::free_balance(5), 80);
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::StakeRemoved {
			staker: 5,
			candidate: 3,
			amount: 20,
		}));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![CandidateInfo { who: 3, deposit: 10, stakers: 1 }]
		);
		// No changes until delay passes
		assert_eq!(
			UnstakingRequests::<Test>::get(5),
			vec![UnstakeRequest { block: 3, amount: 20 }]
		);
		assert_ok!(CollatorStaking::claim(RuntimeOrigin::signed(5)));
		assert_eq!(
			UnstakingRequests::<Test>::get(5),
			vec![UnstakeRequest { block: 3, amount: 20 }]
		);

		initialize_to_block(3);
		assert_ok!(CollatorStaking::claim(RuntimeOrigin::signed(5)));
		assert_eq!(UnstakingRequests::<Test>::get(5), vec![]);
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::StakeClaimed {
			staker: 5,
			amount: 20,
		}));
	});
}

#[test]
fn set_autocompound_percentage() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(AutoCompound::<Test>::get(5), Percent::from_parts(0));
		assert_ok!(CollatorStaking::set_autocompound_percentage(
			RuntimeOrigin::signed(5),
			Percent::from_parts(50)
		));
		assert_eq!(AutoCompound::<Test>::get(5), Percent::from_parts(50));
		System::assert_last_event(RuntimeEvent::CollatorStaking(
			Event::AutoCompoundPercentageSet { staker: 5, percentage: Percent::from_parts(50) },
		));
	});
}

#[test]
fn set_collator_reward_percentage() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(CollatorRewardPercentage::<Test>::get(), Percent::from_parts(20));

		// Invalid origin
		assert_noop!(
			CollatorStaking::set_collator_reward_percentage(
				RuntimeOrigin::signed(5),
				Percent::from_parts(50)
			),
			BadOrigin
		);
		assert_ok!(CollatorStaking::set_collator_reward_percentage(
			RuntimeOrigin::signed(RootAccount::get()),
			Percent::from_parts(50)
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(
			Event::CollatorRewardPercentageSet { percentage: Percent::from_parts(50) },
		));
		assert_eq!(CollatorRewardPercentage::<Test>::get(), Percent::from_parts(50));
	});
}

#[test]
fn set_extra_reward() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(ExtraReward::<Test>::get(), 0);

		// Invalid origin
		assert_noop!(CollatorStaking::set_extra_reward(RuntimeOrigin::signed(5), 10), BadOrigin);

		// Set the reward
		assert_ok!(CollatorStaking::set_extra_reward(
			RuntimeOrigin::signed(RootAccount::get()),
			10
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::ExtraRewardSet {
			amount: 10,
		}));
		assert_eq!(ExtraReward::<Test>::get(), 10);

		// Cannot set to zero
		assert_noop!(
			CollatorStaking::set_extra_reward(RuntimeOrigin::signed(RootAccount::get()), 0),
			Error::<Test>::InvalidExtraReward
		);

		// Revert the changes
		assert_ok!(CollatorStaking::stop_extra_reward(RuntimeOrigin::signed(RootAccount::get()),));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::ExtraRewardRemoved {}));
		assert_eq!(ExtraReward::<Test>::get(), 0);
	});
}

#[test]
fn set_minimum_stake() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(MinStake::<Test>::get(), 2);

		// Invalid origin
		assert_noop!(CollatorStaking::set_minimum_stake(RuntimeOrigin::signed(5), 5), BadOrigin);

		// Set the reward over CandidacyBond
		assert_noop!(
			CollatorStaking::set_minimum_stake(RuntimeOrigin::signed(RootAccount::get()), 1000),
			Error::<Test>::InvalidMinStake
		);

		// Zero is a valid value
		assert_ok!(CollatorStaking::set_minimum_stake(
			RuntimeOrigin::signed(RootAccount::get()),
			0
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewMinStake {
			min_stake: 0,
		}));
		assert_eq!(MinStake::<Test>::get(), 0);

		// Maximum is CandidacyBond
		assert_eq!(CandidacyBond::<Test>::get(), 10);
		assert_ok!(CollatorStaking::set_minimum_stake(
			RuntimeOrigin::signed(RootAccount::get()),
			10
		));
		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewMinStake {
			min_stake: 10,
		}));
		assert_eq!(MinStake::<Test>::get(), 10);
	});
}

#[test]
fn should_not_reward_invulnerables() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::add_invulnerable(RuntimeOrigin::signed(RootAccount::get()), 4));
		assert_eq!(ExtraReward::<Test>::get(), 0);
		assert_eq!(TotalBlocks::<Test>::get(0), (0, 0));
		assert_eq!(CurrentSession::<Test>::get(), 0);
		for block in 1..=9 {
			initialize_to_block(block);
			assert_eq!(CurrentSession::<Test>::get(), 0);
			assert_eq!(TotalBlocks::<Test>::get(0), (block as u32, 0));

			// Transfer the ED first
			Balances::make_free_balance_be(
				&CollatorStaking::account_id(),
				Balances::minimum_balance(),
			);

			// Assume we collected one unit in fees per block
			assert_ok!(Balances::transfer(&1, &CollatorStaking::account_id(), 1, KeepAlive));
			finalize_current_block();
		}

		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 0);
		initialize_to_block(10);
		assert_eq!(CurrentSession::<Test>::get(), 1);
		assert_eq!(TotalBlocks::<Test>::get(1), (1, 0));

		// No StakingRewardReceived should have been emitted if only invulnerable is producing blocks.
		assert!(!System::events().iter().any(|e| {
			match e.event {
				RuntimeEvent::CollatorStaking(Event::StakingRewardReceived { .. }) => true,
				_ => false,
			}
		}));
	});
}

#[test]
fn should_reward_collator() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(4),));
		assert_eq!(ExtraReward::<Test>::get(), 0);
		assert_eq!(Balances::free_balance(&CollatorStaking::account_id()), 0);
		Balances::make_free_balance_be(&CollatorStaking::account_id(), Balances::minimum_balance());
		assert_eq!(TotalBlocks::<Test>::get(0), (0, 0));
		assert_eq!(CurrentSession::<Test>::get(), 0);
		for block in 1..=9 {
			initialize_to_block(block);
			assert_eq!(CurrentSession::<Test>::get(), 0);
			assert_eq!(TotalBlocks::<Test>::get(0), (block as u32, block as u32));

			// Assume we collected one unit in fees per block
			assert_ok!(Balances::transfer(&1, &CollatorStaking::account_id(), 1, KeepAlive));
			finalize_current_block();
		}
		assert_eq!(
			Balances::free_balance(CollatorStaking::account_id()),
			Balances::minimum_balance() + 9
		);
		assert!(!System::events().iter().any(|e| {
			match e.event {
				RuntimeEvent::CollatorStaking(Event::StakingRewardReceived { .. }) => true,
				_ => false,
			}
		}));

		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 9);
		initialize_to_block(10);
		assert_eq!(CurrentSession::<Test>::get(), 1);
		assert_eq!(TotalBlocks::<Test>::get(1), (1, 1));

		finalize_current_block();
		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 0);

		// Total rewards: 9
		// 2 (20%) for collators
		// 8 (80%) for stakers

		// Reward for collator
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 1,
		}));
		// Reward for staker
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 8,
		}));

		assert_eq!(
			Balances::free_balance(&CollatorStaking::account_id()),
			Balances::minimum_balance()
		);
	});
}

#[test]
fn should_reward_collator_with_extra_rewards() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(4),));
		ExtraReward::<Test>::put(1);
		assert_eq!(Balances::free_balance(&CollatorStaking::account_id()), 0);
		Balances::make_free_balance_be(&CollatorStaking::account_id(), Balances::minimum_balance());
		fund_account(CollatorStaking::extra_reward_account_id());

		assert_eq!(TotalBlocks::<Test>::get(0), (0, 0));
		assert_eq!(CurrentSession::<Test>::get(), 0);
		for block in 1..=9 {
			initialize_to_block(block);
			assert_eq!(CurrentSession::<Test>::get(), 0);
			assert_eq!(TotalBlocks::<Test>::get(0), (block as u32, block as u32));

			// Assume we collected one unit in fees per block
			assert_ok!(Balances::transfer(&1, &CollatorStaking::account_id(), 1, KeepAlive));
			finalize_current_block();
		}
		assert_eq!(
			Balances::free_balance(CollatorStaking::account_id()),
			Balances::minimum_balance() + 9
		);
		assert!(!System::events().iter().any(|e| {
			match e.event {
				RuntimeEvent::CollatorStaking(Event::StakingRewardReceived { .. }) => true,
				_ => false,
			}
		}));

		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 9);
		initialize_to_block(10);
		assert_eq!(CurrentSession::<Test>::get(), 1);
		assert_eq!(TotalBlocks::<Test>::get(1), (1, 1));

		finalize_current_block();
		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 0);

		// Total rewards: 18
		// 3 (20%) for collators
		// 15 (80%) for stakers

		// Reward for collator
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 3,
		}));
		// Reward for staker
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 15,
		}));

		assert_eq!(
			Balances::free_balance(&CollatorStaking::account_id()),
			Balances::minimum_balance()
		);
	});
}

#[test]
fn should_reward_collator_with_extra_rewards_and_no_funds() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(4),));
		// This account has no funds
		ExtraReward::<Test>::put(1);
		assert_eq!(Balances::free_balance(&CollatorStaking::account_id()), 0);
		Balances::make_free_balance_be(&CollatorStaking::account_id(), Balances::minimum_balance());

		assert_eq!(TotalBlocks::<Test>::get(0), (0, 0));
		assert_eq!(CurrentSession::<Test>::get(), 0);
		for block in 1..=9 {
			initialize_to_block(block);
			assert_eq!(CurrentSession::<Test>::get(), 0);
			assert_eq!(TotalBlocks::<Test>::get(0), (block as u32, block as u32));

			// Assume we collected one unit in fees per block
			assert_ok!(Balances::transfer(&1, &CollatorStaking::account_id(), 1, KeepAlive));
			finalize_current_block();
		}
		assert_eq!(
			Balances::free_balance(CollatorStaking::account_id()),
			Balances::minimum_balance() + 9
		);
		assert!(!System::events().iter().any(|e| {
			match e.event {
				RuntimeEvent::CollatorStaking(Event::StakingRewardReceived { .. }) => true,
				_ => false,
			}
		}));

		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 9);
		initialize_to_block(10);
		assert_eq!(CurrentSession::<Test>::get(), 1);
		assert_eq!(TotalBlocks::<Test>::get(1), (1, 1));

		finalize_current_block();
		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 0);

		// Total rewards: 9
		// 1 (20%) for collators
		// 8 (80%) for stakers

		// Reward for collator
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 1,
		}));
		// Reward for staker
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 8,
		}));

		assert_eq!(
			Balances::free_balance(&CollatorStaking::account_id()),
			Balances::minimum_balance()
		);
	});
}

#[test]
fn should_reward_collator_with_extra_rewards_and_many_stakers() {
	new_test_ext().execute_with(|| {
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(3),));
		assert_ok!(CollatorStaking::register_as_candidate(RuntimeOrigin::signed(4),));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(2), 4, 40));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 4, 50));
		assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 91));
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 4, deposit: 100, stakers: 3 },
				CandidateInfo { who: 3, deposit: 101, stakers: 2 }
			]
		);

		// Staker 3 will autocompound 40% of its earnings
		AutoCompound::<Test>::insert(3, Percent::from_parts(40));
		ExtraReward::<Test>::put(1);
		assert_eq!(Balances::free_balance(&CollatorStaking::account_id()), 0);
		Balances::make_free_balance_be(&CollatorStaking::account_id(), Balances::minimum_balance());
		fund_account(CollatorStaking::extra_reward_account_id());

		assert_eq!(TotalBlocks::<Test>::get(0), (0, 0));
		assert_eq!(CurrentSession::<Test>::get(), 0);
		for block in 1..=9 {
			initialize_to_block(block);
			assert_eq!(CurrentSession::<Test>::get(), 0);
			assert_eq!(TotalBlocks::<Test>::get(0), (block as u32, block as u32));

			// Assume we collected one unit in fees per block
			assert_ok!(Balances::transfer(&1, &CollatorStaking::account_id(), 1, KeepAlive));
			finalize_current_block();
		}
		assert_eq!(
			Balances::free_balance(CollatorStaking::account_id()),
			Balances::minimum_balance() + 9
		);
		assert!(!System::events().iter().any(|e| {
			match e.event {
				RuntimeEvent::CollatorStaking(Event::StakingRewardReceived { .. }) => true,
				_ => false,
			}
		}));

		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 9);
		initialize_to_block(10);
		assert_eq!(CurrentSession::<Test>::get(), 1);
		assert_eq!(TotalBlocks::<Test>::get(1), (1, 1));

		finalize_current_block();
		assert_eq!(ProducedBlocks::<Test>::get(0, 4), 0);

		// Total rewards: 18
		// 3 (20%) for collators
		// 15 (80%) for stakers
		//  - Staker 2 -> 40% = 6
		//  - Staker 3 -> 50% = 7
		//  - Staker 4 (collator) -> 10% = 1

		// Reward for collator
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 3,
		}));
		// Reward for stakers
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 2,
			amount: 6,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 3,
			amount: 7,
		}));
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakingRewardReceived {
			staker: 4,
			amount: 1,
		}));

		// Check that staker 3 added 40% of its earnings via autocompound.
		System::assert_has_event(RuntimeEvent::CollatorStaking(Event::StakeAdded {
			staker: 3,
			candidate: 4,
			amount: 2,
		}));

		// Check after adding the stake via autocompound the candidate list is sorted.
		assert_eq!(
			CandidateList::<Test>::get(),
			vec![
				CandidateInfo { who: 3, deposit: 101, stakers: 2 },
				CandidateInfo { who: 4, deposit: 102, stakers: 3 },
			]
		);

		// We could not split the reward evenly, so what remains will be part of the next reward.
		// This it not critical, as amounts are very low.
		assert_eq!(
			Balances::free_balance(&CollatorStaking::account_id()),
			Balances::minimum_balance() + 1
		);
	});
}

#[test]
fn stop_extra_reward() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		fund_account(CollatorStaking::extra_reward_account_id());
		assert_eq!(ExtraReward::<Test>::get(), 0);

		// Cannot stop if already zero
		assert_noop!(
			CollatorStaking::stop_extra_reward(RuntimeOrigin::signed(RootAccount::get())),
			Error::<Test>::ExtraRewardAlreadyDisabled
		);

		// Now we can stop it
		assert_ok!(CollatorStaking::set_extra_reward(RuntimeOrigin::signed(RootAccount::get()), 2));
		assert_ok!(CollatorStaking::stop_extra_reward(RuntimeOrigin::signed(RootAccount::get())));

		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::ExtraRewardRemoved {}));
		assert_eq!(ExtraReward::<Test>::get(), 0);
	});
}

#[test]
fn top_up_extra_rewards() {
	new_test_ext().execute_with(|| {
		initialize_to_block(1);

		assert_eq!(Balances::free_balance(&CollatorStaking::extra_reward_account_id()), 0);

		// Cannot fund with an amount equal to zero.
		assert_noop!(
			CollatorStaking::top_up_extra_rewards(RuntimeOrigin::signed(1), 0),
			Error::<Test>::InvalidFundingAmount
		);

		// Cannot fund if total balance less than ED.
		assert!(CollatorStaking::top_up_extra_rewards(RuntimeOrigin::signed(1), 1).is_err());

		// Now we can stop it
		assert_ok!(CollatorStaking::top_up_extra_rewards(RuntimeOrigin::signed(1), 10));

		System::assert_last_event(RuntimeEvent::CollatorStaking(Event::ExtraRewardPotFunded {
			pot: CollatorStaking::extra_reward_account_id(),
			amount: 10,
		}));
		assert_eq!(Balances::free_balance(&CollatorStaking::extra_reward_account_id()), 10);
	});
}
