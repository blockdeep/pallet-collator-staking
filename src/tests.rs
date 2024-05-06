use crate as collator_staking;
use crate::{
    mock::*, CandidacyBond, CandidateInfo, CandidateList, CollatorRewardPercentage,
    DesiredCandidates, Error, Event, Invulnerables, LastAuthoredBlock, MinStake,
};
use crate::{Stake, UnstakeRequest, UnstakingRequests};
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
    let key = MockSessionKeys {
        aura: UintAuthorityId(acc),
    };
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
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(ii),
        ));
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
        assert_eq!(DesiredCandidates::<Test>::get(), 2);
        assert_eq!(CandidacyBond::<Test>::get(), 10);
        assert_eq!(MinStake::<Test>::get(), 2);
        assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
        assert_eq!(
            CollatorRewardPercentage::<Test>::get(),
            Percent::from_parts(20)
        );
        // The minimum balance should have been minted
        assert_eq!(
            Balances::free_balance(CollatorStaking::account_id()),
            Balances::minimum_balance()
        );
        // genesis should sort input
        assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
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
fn it_should_set_invulnerables_even_with_some_invalid() {
    new_test_ext().execute_with(|| {
        initialize_to_block(1);
        assert_eq!(Invulnerables::<Test>::get(), vec![1, 2]);
        let new_with_invalid = vec![1, 4, 3, 42, 2];

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
        assert_noop!(
            CollatorStaking::add_invulnerable(RuntimeOrigin::signed(1), new),
            BadOrigin
        );

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
                let key = MockSessionKeys {
                    aura: UintAuthorityId(ii),
                };
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

        assert_ok!(CollatorStaking::add_invulnerable(
            RuntimeOrigin::signed(RootAccount::get()),
            4
        ));
        System::assert_last_event(RuntimeEvent::CollatorStaking(Event::InvulnerableAdded {
            account_id: 4,
        }));
        assert_ok!(CollatorStaking::add_invulnerable(
            RuntimeOrigin::signed(RootAccount::get()),
            3
        ));
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
        assert_noop!(
            CollatorStaking::remove_invulnerable(RuntimeOrigin::signed(1), 3),
            BadOrigin
        );
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

        assert_ok!(CollatorStaking::add_invulnerable(
            RuntimeOrigin::signed(RootAccount::get()),
            3
        ));
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

        assert_ok!(CollatorStaking::add_invulnerable(
            RuntimeOrigin::signed(RootAccount::get()),
            4
        ));
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
            7
        ));
        System::assert_last_event(RuntimeEvent::CollatorStaking(Event::NewDesiredCandidates {
            desired_candidates: 7,
        }));
        assert_eq!(DesiredCandidates::<Test>::get(), 7);

        // rejects bad origin
        assert_noop!(
            CollatorStaking::set_desired_candidates(RuntimeOrigin::signed(1), 8),
            BadOrigin
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
        assert_noop!(
            CollatorStaking::set_candidacy_bond(RuntimeOrigin::signed(1), 8),
            BadOrigin
        );

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

        let candidate_3 = CandidateInfo {
            who: 3,
            deposit: 10,
        };

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

        let candidate_3 = CandidateInfo {
            who: 3,
            deposit: 10,
        };
        let candidate_4 = CandidateInfo {
            who: 4,
            deposit: 10,
        };
        let candidate_5 = CandidateInfo {
            who: 5,
            deposit: 10,
        };

        register_candidates(3..=5);

        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                candidate_5.clone(),
                candidate_4.clone(),
                candidate_3.clone()
            ]
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
            vec![
                candidate_5.clone(),
                candidate_4.clone(),
                candidate_3.clone()
            ]
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
            vec![
                candidate_5.clone(),
                candidate_4.clone(),
                candidate_3.clone()
            ]
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
        let new_candidate_5 = CandidateInfo {
            who: 5,
            deposit: 30,
        };
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                candidate_4.clone(),
                candidate_3.clone(),
                new_candidate_5.clone()
            ]
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
        let addition = CandidateInfo {
            who: 3,
            deposit: 10,
        };
        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
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
fn cannot_take_candidate_slot_if_keys_not_registered() {
    new_test_ext().execute_with(|| {
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
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

        let actual_candidates = CandidateList::<Test>::get()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
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
        assert_ok!(CollatorStaking::take_candidate_slot(
            RuntimeOrigin::signed(3),
            20u64.into(),
            4
        ));
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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::stake(
            RuntimeOrigin::signed(3),
            3,
            60u64.into()
        ));
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
        assert_ok!(CollatorStaking::stake(
            RuntimeOrigin::signed(3),
            3,
            60u64.into()
        ));

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

        let candidate_3 = CandidateInfo {
            who: 3,
            deposit: 40,
        };
        let candidate_4 = CandidateInfo {
            who: 4,
            deposit: 35,
        };
        let candidate_5 = CandidateInfo {
            who: 5,
            deposit: 60,
        };
        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
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

        let unstake_request = UnstakeRequest {
            block: 6,
            amount: 10,
        };
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

        // Nothing panics, no reward when no ED in balance
        Authorship::on_initialize(1);
        // put some money into the pot at ED
        Balances::make_free_balance_be(&CollatorStaking::account_id(), 5);
        // 4 is the default author.
        assert_eq!(Balances::free_balance(4), 100);
        register_candidates(4..=4);
        // triggers `note_author`
        Authorship::on_initialize(1);

        // tuple of (id, deposit).
        let collator = CandidateInfo {
            who: 4,
            deposit: 10,
        };

        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
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
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));

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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));

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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));
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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));
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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));

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

        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));

        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 60));

        initialize_to_block(5);

        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(4), 4, 70));
        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(3), 3, 70));

        initialize_to_block(5);

        // candidate 5 saw it was outbid and wants to take back its bid, but
        // not entirely so they still keep their place in the candidate list
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
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        initialize_to_block(10);
        assert_eq!(CandidateList::<Test>::get().iter().count(), 2);
        initialize_to_block(20);
        assert_eq!(SessionChangeBlock::get(), 20);
        // 4 authored this block, gets to stay 3 was kicked
        assert_eq!(CandidateList::<Test>::get().iter().count(), 1);
        // 3 will be kicked after 1 session delay
        assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 3, 4]);
        // tuple of (id, deposit).
        let collator = CandidateInfo {
            who: 4,
            deposit: 10,
        };
        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![collator]
        );
        assert_eq!(LastAuthoredBlock::<Test>::get(4), 20);
        initialize_to_block(30);
        // 3 gets kicked after 1 session delay
        assert_eq!(SessionHandlerCollators::get(), vec![1, 2, 4]);
        // kicked collator gets funds back
        assert_eq!(Balances::free_balance(3), 100);
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
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(5)
        ));
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
        let collator = CandidateInfo {
            who: 3,
            deposit: 10,
        };
        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![collator]
        );
        assert_eq!(LastAuthoredBlock::<Test>::get(4), 20);

        initialize_to_block(30);
        // 3 gets kicked after 1 session delay
        assert_eq!(SessionHandlerCollators::get(), vec![3]);
        // kicked collator gets funds back
        assert_eq!(Balances::free_balance(5), 100);
    });
}

#[test]
fn should_kick_invulnerables_from_candidates_on_session_change() {
    new_test_ext().execute_with(|| {
        assert_eq!(CandidateList::<Test>::get().iter().count(), 0);
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(3)
        ));
        assert_ok!(CollatorStaking::register_as_candidate(
            RuntimeOrigin::signed(4)
        ));
        assert_eq!(Balances::free_balance(3), 90);
        assert_eq!(Balances::free_balance(4), 90);
        assert_ok!(CollatorStaking::set_invulnerables(
            RuntimeOrigin::signed(RootAccount::get()),
            vec![1, 2, 3]
        ));

        // tuple of (id, deposit).
        let collator_3 = CandidateInfo {
            who: 3,
            deposit: 10,
        };
        let collator_4 = CandidateInfo {
            who: 4,
            deposit: 10,
        };

        let actual_candidates = CandidateList::<Test>::get()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(actual_candidates, vec![collator_4.clone(), collator_3]);
        assert_eq!(Invulnerables::<Test>::get(), vec![1, 2, 3]);

        // session change
        initialize_to_block(10);
        // 3 is removed from candidates
        assert_eq!(
            CandidateList::<Test>::get()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
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
    let mut t = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    let invulnerables = vec![1, 1];

    let collator_staking = collator_staking::GenesisConfig::<Test> {
        desired_candidates: 2,
        candidacy_bond: 10,
        min_stake: 1,
        invulnerables,
        collator_reward_percentage: Percent::from_parts(20),
    };
    // collator selection must be initialized before session.
    collator_staking.assimilate_storage(&mut t).unwrap();
}

#[test]
fn cannot_stake_if_not_candidate() {
    new_test_ext().execute_with(|| {
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
        assert_eq!(CandidateList::<Test>::get()[0].deposit, 10);

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
                CandidateInfo {
                    who: 4,
                    deposit: 10
                },
                CandidateInfo {
                    who: 3,
                    deposit: 10
                },
            ]
        );
        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 3, 2));
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                CandidateInfo {
                    who: 4,
                    deposit: 10
                },
                CandidateInfo {
                    who: 3,
                    deposit: 12
                },
            ]
        );

        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 4, 5));
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                CandidateInfo {
                    who: 3,
                    deposit: 12
                },
                CandidateInfo {
                    who: 4,
                    deposit: 15
                },
            ]
        );

        register_candidates(5..=5);
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                CandidateInfo {
                    who: 5,
                    deposit: 10
                },
                CandidateInfo {
                    who: 3,
                    deposit: 12
                },
                CandidateInfo {
                    who: 4,
                    deposit: 15
                },
            ]
        );
        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 3));
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                CandidateInfo {
                    who: 3,
                    deposit: 12
                },
                CandidateInfo {
                    who: 5,
                    deposit: 13
                },
                CandidateInfo {
                    who: 4,
                    deposit: 15
                },
            ]
        );
        assert_ok!(CollatorStaking::stake(RuntimeOrigin::signed(5), 5, 7));
        assert_eq!(
            CandidateList::<Test>::get(),
            vec![
                CandidateInfo {
                    who: 3,
                    deposit: 12
                },
                CandidateInfo {
                    who: 4,
                    deposit: 15
                },
                CandidateInfo {
                    who: 5,
                    deposit: 20
                },
            ]
        );
    });
}
