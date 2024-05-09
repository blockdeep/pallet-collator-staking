#![allow(unused_parens)]
#![allow(unused_imports)]

use frame_support::{
	traits::Get,
	weights::{constants::RocksDbWeight, Weight},
};
use sp_std::marker::PhantomData;

// The weight info trait for `pallet_collator_staking`.
pub trait WeightInfo {
	fn set_invulnerables(_b: u32) -> Weight;
	fn add_invulnerable(_b: u32, _c: u32) -> Weight;
	fn remove_invulnerable(_b: u32) -> Weight;
	fn set_desired_candidates() -> Weight;
	fn set_candidacy_bond() -> Weight;
	fn register_as_candidate(_c: u32) -> Weight;
	fn leave_intent(_c: u32) -> Weight;
	fn take_candidate_slot(_c: u32) -> Weight;
	fn note_author() -> Weight;
	fn new_session(_c: u32, _r: u32) -> Weight;
	fn stake(_c: u32) -> Weight;
}

impl WeightInfo for () {
	fn set_invulnerables(_b: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn add_invulnerable(_b: u32, _c: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn remove_invulnerable(_b: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn set_desired_candidates() -> Weight {
		Weight::from_parts(0, 0)
	}

	fn set_candidacy_bond() -> Weight {
		Weight::from_parts(0, 0)
	}

	fn register_as_candidate(_c: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn leave_intent(_c: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn take_candidate_slot(_c: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn note_author() -> Weight {
		Weight::from_parts(0, 0)
	}

	fn new_session(_c: u32, _r: u32) -> Weight {
		Weight::from_parts(0, 0)
	}

	fn stake(_c: u32) -> Weight {
		Weight::from_parts(0, 0)
	}
}
