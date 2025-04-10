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

sp_api::decl_runtime_apis! {
	/// This runtime api allows to query:
	/// - The main pallet's pot account.
	/// - The extra rewards pot account.
	/// - Accumulated rewards for an account.
	/// - Whether a given account has rewards pending to be claimed or not.
	/// - The current candidates and their corresponding stake.
	///
	/// Sample implementation:
	/// ```ignore
	/// impl pallet_collator_staking::CollatorStakingApi<Block, AccountId, Balance> for Runtime {
	///    fn main_pot_account() -> AccountId {
	///        CollatorStaking::account_id()
	///    }
	///    fn extra_reward_pot_account() -> AccountId {
	///        CollatorStaking::extra_reward_account_id()
	///    }
	///    fn total_rewards(account: AccountId) -> Balance {
	///        CollatorStaking::calculate_unclaimed_rewards(&account)
	///    }
	///    fn should_claim(account: AccountId) -> bool {
	///        !CollatorStaking::staker_has_claimed(&account)
	///    }
	///	   fn candidates() -> Vec<(AccountId, Balance)> {
	///        CollatorStaking::candidates()
	///    }
	/// }
	/// ```
	pub trait CollatorStakingApi<AccountId, Balance>
	where
		AccountId: codec::Codec,
		Balance: codec::Codec,
	{
		/// Queries the main pot account.
		fn main_pot_account() -> AccountId;

		/// Queries the extra reward pot account.
		fn extra_reward_pot_account() -> AccountId;

		/// Gets the total accumulated rewards.
		fn total_rewards(account: AccountId) -> Balance;

		/// Returns true if user should claim rewards.
		fn should_claim(account: AccountId) -> bool;

		/// Returns a list with all candidates and their stake.
		fn candidates() -> sp_std::vec::Vec<(AccountId, Balance)>;
	}
}
