// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::Duration;

use cucumber::{then, when};
use integration_tests::TariWorld;

pub mod base_node_steps;
pub mod p2pool_steps;

pub const CONFIRMATION_PERIOD: u64 = 4;
pub const TWO_MINUTES_WITH_HALF_SECOND_SLEEP: u64 = 240;
pub const HALF_SECOND: u64 = 500;

#[when(expr = "I wait {int} seconds")]
async fn wait_seconds_and_continue(_world: &mut TariWorld, seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

#[then(expr = "I wait {int} seconds and stop")]
async fn wait_seconds_and_stop(_world: &mut TariWorld, seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

#[then(regex = r"I receive an error containing '(.*)'")]
async fn receive_an_error(world: &mut TariWorld, error: String) {
    match world.errors.pop_back() {
        Some(err) => assert_eq!(err, error),
        None => panic!("Should have received an error"),
    };
}
