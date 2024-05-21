 # Collator Staking pallet.

 A simple DPoS pallet for collators in a parachain.

 ## Overview

 The Collator Staking pallet provides DPoS functionality to manage collators of a parachain.
 It allows stakers to stake their tokens to back collators, and receive rewards proportionately.
 There is no slashing in place. If a collator does not produce blocks as expected,
 they are removed from the collator set.

## Compatibility

This pallet is compatible with [polkadot version 1.11.0](https://github.com/paritytech/polkadot-sdk/releases/tag/polkadot-v1.11.0).

## How it works

This pallet maintains two separate lists of accounts subject to produce blocks:

* `Invulnerables`: accounts that are always selected to become collators. They can only be removed by the pallet's authority.
**Invulnerables do not receive staking rewards, yet blocks produced by them count for rewards**.
* `Candidates`: accounts that compete to be part of the collator set by getting enough delegated stake.

### Rewards
The main idea is that candidates are incentivized to compete to become collators so that they can access staking rewards.
These rewards are composed of:

* An optional per-block flat amount coming from a different pot (for example, Treasury).
* Fees and tips collected during all blocks produced in the current session.

Bear in mind that rewards are generated out of existing funds on the blockchain, and **by no means an inflationary mechanism is utilized**.

Rewards must be distributed so that all stakeholders are incentivized to participate:
* Candidates compete to become collators.
* Collators must not misbehave and produce blocks honestly so that they increase the chances to produce more blocks and this way be more attractive for other users to stake on.
* Stakers must select wisely the candidates they want to deposit the stake on, hence determining the best possible candidates that are likely to become collators.
* Rewards must be proportionally distributed among collators and stakers when the session ends so that:
  - They are assigned to collators depending on the amount of blocks produced in a given session.
  - Collators receive an exclusive percentage of them for collating.
  - Stakers receive the remaining proportionally to the amount staked in a given collator.

### Staking
Any account in the parachain can deposit a stake on a given candidate so that this way it can possess a higher deposit than
solely with its own candidacy bond and hence increase its possibilities to be selected as a collator.

If the candidate receives stake from users, it is incentivized to remain online and behave honestly, as this way it will have
access to staking rewards, and its stakers will be incentivized to retain the stake, as they would be rewarded.

### Unstaking
When a given staker wishes to unstake, there must exist a delay: the staker will have to wait for a given number of blocks
before his funds are released, and it will get no rewards in the meantime. This is done so that stakers are incentivized to
discriminate between good and bad candidates.

### Auto-compounding
Users can also select the percentage of rewards that will be auto-compounded. If the selected percentage is greater than zero,
part of the rewards will be re-invested as stake in the collator that obtained the rewards.

### Extra pallets

#### Pallet session
This pallet is tightly coupled to [pallet-session](https://github.com/paritytech/polkadot-sdk/tree/master/cumulus/pallets/collator-selection), as it plays
the role of the session manager by deciding the next collator set. Also, rewards are assigned when sessions end and start counting when sessions start.

#### Pallet authorship
This pallet is tightly coupled to [pallet-authorship](https://github.com/paritytech/polkadot-sdk/tree/master/substrate/frame/authorship) by requiring
to be subscribed to block production authorship so that it can assign rewards to collators and their stakers accordingly.


## Hooks

This pallet uses the following hooks:

* `on_idle`: returns funds to stakers when a candidate leaves. This is a best-effort process, based on whether the block has sufficient unused space left.
* `on_initialize`: during the first few blocks after the session starts one collator per block will be rewarded, along with its stakers. This is a process that can potentially consume a significant amount of weight.

## Configuration

### Types

| Parameter                | Description                                                                                          |
|--------------------------|------------------------------------------------------------------------------------------------------|
| `RuntimeEvent`           | The overarching event type.                                                                          |
| `Currency`               | The currency mechanism.                                                                              |
| `UpdateOrigin`           | Origin that can dictate updating parameters of this pallet.                                          |
| `PotId`                  | Account Identifier from which the internal pot is generated.                                         |
| `ExtraRewardPotId`       | Account Identifier from which the extra reward pot is generated.                                     |
| `MinEligibleCollators`   | Minimum number eligible collators including Invulnerables.                                           |
| `MaxInvulnerables`       | Maximum number of invulnerables.                                                                     |
| `KickThreshold`          | Candidates will be removed from active collator set, if block is not produced within this threshold. |
| `CollatorId`             | A stable ID for a collator.                                                                          |
| `CollatorIdOf`           | A conversion from account ID to collator ID.                                                         |
| `CollatorRegistration`   | Validate a collator is registered.                                                                   |
| `MaxStakedCandidates`    | Maximum candidates a staker can stake on.                                                            |
| `MaxStakers`             | Maximum stakers per candidate.                                                                       |
| `CollatorUnstakingDelay` | Number of blocks to wait before returning the bond by a collator.                                    |
| `UserUnstakingDelay`     | Number of blocks to wait before returning the stake by a user.                                       |
| `WeightInfo`             | Information on runtime weights.                                                                      |


### Setup considerations

While it is important to set `MaxStakedCandidates` and `MaxStakers` to a reasonably high value, bear in mind this may significantly
impact in weight consumption. We recommend to measure the weights and set values that in the worst case do not occupy more
than a sane limit, like 20% or 30% of the block's total weight.

Concerning `DesiredCandidates`, bear in mind that the number of eligible candidates must be lower than the worst-case session length, as otherwise
not all collators (and their stakers) will receive the rewards.


## Extrinsics

<details>
<summary><h3>set_invulnerables</h3></summary>

Set the list of invulnerable (fixed) collators. These collators must do some
preparation, namely to have registered session keys.

The call will remove any accounts that have not registered keys from the set. That is,
it is non-atomic; the caller accepts all `AccountId`s passed in `new` _individually_ as
acceptable Invulnerables, and is not proposing a _set_ of new Invulnerables.

This call does not maintain mutual exclusivity of `Invulnerables` and `Candidates`. It
is recommended to use a batch of `add_invulnerable` and `remove_invulnerable` instead. A
`batch_all` can also be used to enforce atomicity. If any candidates are included in
`new`, they should be removed with `remove_invulnerable_candidate` after execution.

Must be called by the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `new` - The invulnerable list.

### Errors:
* `TooFewEligibleCollators` - Minimum required collators not met.
* `TooManyInvulnerables` - Too many invulnerables in the list.

</details>

<details>
<summary><h3>set_desired_candidates</h3></summary>

Set the ideal number of collators. If lowering this number, then the
number of running collators could be higher than this figure. Aside from that edge case,
there should be no other way to have more candidates than the desired number.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `max` - The number of desired candidates.

### Errors:
* `TooManyDesiredCandidates` - The number of desired candidates is too high.

</details>

<details>
<summary><h3>set_candidacy_bond</h3></summary>

Set the candidacy bond amount, which represents the required amount to reserve for an account to become a candidate.
The candidacy bond does not count as stake.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `bond` – The new candidacy bond.

### Errors:
* `InvalidCandidacyBond` - The candidacy bond cannot be lower than `MinStake`.

</details>

<details>
<summary><h3>register_as_candidate</h3></summary>

Register this account as a collator candidate. The account must (a) already have
registered session keys and (b) be able to reserve the `CandidacyBond`.
The `CandidacyBond` amount is automatically reserved from the balance of the caller.

This call is not available to `Invulnerable` collators.

### Parameters:
* `origin` – Origin for the call.

### Errors:
* `TooManyCandidates` - The candidate list is already full. In this case `take_candidate_slot` must be used.
* `AlreadyInvulnerable` - The account is already an invulnerable.
* `NoAssociatedCollatorId` - Collator ID was not found.
* `CollatorNotRegistered` - Collator was found but its keys are not registered in the session pallet.
* `AlreadyCandidate` - The account is already registered.

</details>

<details>
<summary><h3>leave_intent</h3></summary>

Deregister `origin` as a collator candidate. No rewards will be delivered to this
candidate and its stakers after this moment.

This call will fail if the total number of candidates would drop below
`MinEligibleCollators`.

### Parameters:
* `origin` – Origin for the call.

### Errors:
* `TooFewEligibleCollators` - There would be not enough collators if the candidate leaves.
* `NotCandidate` - The account is not a candidate.
* `TooManyUnstakingRequests` - The candidate has too many unstaking requests to be claimed and cannot create another one.

</details>

<details>
<summary><h3>add_invulnerable</h3></summary>

Add a new account `who` to the list of `Invulnerables` collators. `who` must have
registered session keys. If `who` is a candidate, it will be removed.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `who` – The new invulnerable.

### Errors:
* `CollatorNotRegistered` - No session keys registered.
* `AlreadyInvulnerable` - The account is already an invulnerable and cannot be added twice.
* `TooManyInvulnerables` - There cannot be more invulnerables at the moment.

</details>

<details>
<summary><h3>remove_invulnerable</h3></summary>

Remove an account `who` from the list of `Invulnerables` collators. `Invulnerables` must
be sorted.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `who` – The invulnerable to remove.

### Errors:
* `TooFewEligibleCollators` - There would be not enough collators if the invulnerable is removed.
* `NotInvulnerable` - The account is not an invulnerable.

</details>

<details>
<summary><h3>take_candidate_slot</h3></summary>

The caller `origin` replaces a candidate `target` in the collator candidate list by
reserving the [`CandidacyBond`] and adding stake to itself. The stake added by the caller
must be greater than the existing stake of the target it is trying to replace.

This call will fail if the caller is already a collator candidate or invulnerable, the
caller does not have registered session keys, the target is not a collator candidate,
the list of candidates is not full,
and/or the candidacy bond or stake cannot be reserved.

### Parameters:
* `origin` – Origin for the call.
* `stake` – The amount of stake to add to itself.
* `target` – The candidate to take the slot of.

### Errors:
* `AlreadyInvulnerable` - There caller is an invulnerable.
* `AlreadyCandidate` - The caller is already a candidate.
* `NoAssociatedCollatorId` - Candidate's collator ID not found.
* `CollatorNotRegistered` - The caller did not register the session keys.
* `CanRegister` - The candidate list is not full. Instead, call `register_as_candidate`.
* `InsufficientStake` - The staked amount is less than the target's, or the caller does not have enough funds.

</details>

<details>
<summary><h3>stake</h3></summary>

Allows a user to stake on a collator candidate.

The call will fail if:
- `origin` does not have the at least `MinStake` deposited in the candidate.
- `candidate` is not in the `CandidateList`.

### Parameters:
* `origin` – Origin for the call.
* `candidate` – The candidate to stake on.
* `stake` – The stake to add to the candidate.

### Errors:
* `NotCandidate` - The account is not a candidate.
* `TooManyStakedCandidates` - Cannot have stake in more than `MaxStakedCandidates` different candidates.
* `TooManyStakers` - `candidate` cannot receive stake from more than `MaxStakers` different stakers.

</details>

<details>
<summary><h3>unstake_from</h3></summary>

Removes stake from a collator candidate.

If the candidate is an active collator, the caller will get the funds after a delay. Otherwise,
funds will be returned immediately.

The candidate will have its position in the `CandidateList` updated.

### Parameters:
* `origin` – Origin for the call.
* `candidate` – The candidate to unstake from.

### Errors:
* `NotCandidate` - The account is not a candidate.
* `NothingToUnstake` - The caller does not have stake in the candidate.
* `TooManyUnstakingRequests` - The caller's list of unstaking requests is full.

</details>

<details>
<summary><h3>unstake_all</h3></summary>

Removes all stake of a user from all candidates.

The delay in amount refunded is based on whether the candidates are active collators or not.

### Parameters:
* `origin` – Origin for the call.

### Errors:
* `TooManyUnstakingRequests` - The caller's list of unstaking requests is full.

</details>

<details>
<summary><h3>claim</h3></summary>

Claims all pending `UnstakeRequest` for a given account.

### Parameters:
* `origin` – Origin for the call.

</details>

<details>
<summary><h3>set_autocompound_percentage</h3></summary>

Sets the percentage of rewards that should be auto-compounded.

### Parameters:
* `origin` – Origin for the call.
* `percent` – The percentage of rewards to be compounded for all candidates.

</details>

<details>
<summary><h3>set_collator_reward_percentage</h3></summary>

Sets the percentage of rewards that collators will take for producing blocks.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `percent` – The percentage of rewards for collators.

</details>

<details>
<summary><h3>set_extra_reward</h3></summary>

Sets the extra rewards for producing blocks. Once the session finishes, the provided amount times
the total number of blocks produced during the session will be transferred from the given account
to the pallet's pot account to be distributed as rewards.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `extra_reward` – The raw per-block extra reward to be delivered when sessions end.

### Errors:
* `InvalidExtraReward` - If the extra reward is zero. If you want to disable extra rewards use `stop_extra_reward`.

</details>

<details>
<summary><h3>set_minimum_stake</h3></summary>

Sets minimum amount that can be staked on a candidate.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.
* `new_min_stake` – The minimum per-candidate amount that can be staked.

### Errors:
* `InvalidMinStake` - If the new minimum stake is greater than the candidacy bond.

</details>

<details>
<summary><h3>stop_extra_reward</h3></summary>

Stops the extra rewards.

The origin for this call must be the `UpdateOrigin`.

### Parameters:
* `origin` – Origin for the call. It must be `UpdateOrigin`.

### Errors:
* `ExtraRewardAlreadyDisabled` - If extra rewards are already disabled.

</details>

<details>
<summary><h3>top_up_extra_rewards</h3></summary>

Funds the extra reward pot account.

Will fail if the caller does not have enough funds.

### Parameters:
* `origin` – Origin for the call.
* `amount` – Amount to transfer to the pot.

### Errors:
* `InvalidFundingAmount` - If amount to transfer is zero.

</details>



## How to add `pallet-collator-staking` to the runtime

:information_source: The pallet is compatible with Substrate version **[polkadot-v1.11.0](https://github.com/paritytech/polkadot-sdk/tree/polkadot-v1.11.0).**

### Runtime's `Cargo.toml`

Add `pallet-collator-staking` to the dependencies:

```toml
[dependencies]
# --snip--
pallet-collator-staking = { version = "1.11.0", default-features = false, git = "https://github.com/blockdeep/pallet-collator-staking.git" }
# --snip--
```

Update the runtime's `std`, `runtime-benchmarks` and `try-runtime` features:
```toml
std = [
    # --snip--
    "pallet-collator-staking/std",
    # --snip--
]

runtime-benchmarks = [
    # --snip--
    "pallet-collator-staking/runtime-benchmarks",
    # --snip--
]

try-runtime = [
    # --snip--
    "pallet-collator-staking/try-runtime",
    # --snip--
]
```

Configure the collator staking pallet.
```rust
parameter_types! {
    pub const PotId: PalletId = PalletId(*b"StakePot");
    pub const ExtraRewardPotId: PalletId = PalletId(*b"ExtraPot");
    pub const MaxCandidates: u32 = 100;
    pub const MinEligibleCollators: u32 = 1;
    pub const MaxInvulnerables: u32 = 20;
    pub const MaxStakers: u32 = 200;
    pub const KickThreshold: u32 = 5 * Period::get();
}

impl pallet_collator_staking::Config for Runtime {
 type RuntimeEvent = RuntimeEvent;
 type Currency = Balances;
 type RuntimeHoldReason = RuntimeHoldReason;
 type UpdateOrigin = RootOrCouncilTwoThirdsMajority;
 type PotId = PotId;
 type ExtraRewardPotId = ExtraRewardPotId;
 type MaxCandidates = MaxCandidates;
 type MinEligibleCollators = MinEligibleCollators;
 type MaxInvulnerables = MaxInvulnerables;
 // should be a multiple of session or things will get inconsistent
 type KickThreshold = KickThreshold;
 type CollatorId = <Self as frame_system::Config>::AccountId;
 type CollatorIdOf = pallet_collator_staking::IdentityCollator;
 type CollatorRegistration = Session;
 type MaxStakedCandidates = ConstU32<16>;
 type MaxStakers = MaxStakers;
 type CollatorUnstakingDelay = ConstU32<20>;
 type UserUnstakingDelay = ConstU32<10>;
 type WeightInfo = pallet_collator_staking::weights::SubstrateWeight<Runtime>;
}
```

Add the pallet to the `construct_runtime` macro call:
```rust
construct_runtime!(
    pub struct Runtime {
        // --snip---
        CollatorStaking: pallet_collator_staking,
        // --snip---
    }
);
```

And to the benchmark list:
```rust
#[cfg(feature = "runtime-benchmarks")]
mod benches {
    frame_benchmarking::define_benchmarks!(
        [frame_system, SystemBench::<Runtime>]
        [pallet_balances, Balances]
        [pallet_session, SessionBench::<Runtime>]
        [pallet_timestamp, Timestamp]
        // --snip---
        [pallet_collator_staking, CollatorStaking]
        // --snip---
   );
}
```

Register the pallet as a handler in `pallet-authorship` to receive notifications for block production and authorship.
```rust
impl pallet_authorship::Config for Runtime {
    // --snip--
	type EventHandler = (CollatorStaking,);
}
```

Set the pallet as `pallet-session`'s manager:
```rust
impl pallet_session::Config for Runtime {
	// --snip--
    type SessionManager = CollatorStaking;
    // --snip--
}
```
