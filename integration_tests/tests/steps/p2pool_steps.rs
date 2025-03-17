// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use cucumber::{given, when};
use integration_tests::{
    miner::{mine_and_submit_tari_blocks, verify_block_height},
    p2pool_process::{restart_node, shut_down_node, spawn_p2pool_node_and_wait_for_start, verify_peer_connected},
    TariWorld,
};
use log::*;
use tokio::time::Duration;

pub const LOG_TARGET: &str = "cucumber::p2pool_steps";

#[given(expr = "I have a p2pool seed node {word} in squad {word} connected to base node {word}")]
#[when(expr = "I have a p2pool seed node {word} in squad {word} connected to base node {word}")]
async fn start_p2pool_seed_node(world: &mut TariWorld, p2pool_name: String, squad: String, base_node_name: String) {
    if let Err(err) = spawn_p2pool_node_and_wait_for_start(world, true, p2pool_name, squad, base_node_name).await {
        let msg = format!("start_p2pool_seed_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I have a p2pool node {word} in squad {word} connected to base node {word}")]
#[when(expr = "I have a p2pool node {word} in squad {word} connected to base node {word}")]
async fn start_p2pool_node(world: &mut TariWorld, p2pool_name: String, squad: String, base_node_name: String) {
    if let Err(err) = spawn_p2pool_node_and_wait_for_start(world, false, p2pool_name, squad, base_node_name).await {
        let msg = format!("start_p2pool_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I add {int} blocks to p2pool node {word}")]
#[when(expr = "I add {int} blocks to p2pool node {word}")]
async fn add_blocks_to_p2pool_node(world: &mut TariWorld, number_of_blocks: u64, p2pool_name: String) {
    if let Err(err) = mine_and_submit_tari_blocks(world, number_of_blocks, p2pool_name).await {
        let msg = format!("add_blocks_to_p2pool_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "p2pool node {} stats is at height {int}")]
#[when(expr = "p2pool node {} stats is at height {int}")]
async fn verify_p2pool_block_height(world: &mut TariWorld, p2pool_name: String, height: u64) {
    if let Err(err) = verify_block_height(world, p2pool_name, height).await {
        let msg = format!("verify_p2pool_block_height: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "p2pool node {} stats shows connected to peer {}")]
#[when(expr = "p2pool node {} stats shows connected to peer {}")]
async fn verify_p2pool_peer_connected(world: &mut TariWorld, p2pool_name: String, peer_name: String) {
    if let Err(err) = verify_peer_connected(world, p2pool_name, peer_name).await {
        let msg = format!("verify_p2pool_peer_connected: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I stop p2pool node {}")]
#[when(expr = "I stop p2pool node {}")]
async fn shut_down_p2pool_node(world: &mut TariWorld, p2pool_name: String) {
    if let Err(err) = shut_down_node(world, p2pool_name).await {
        let msg = format!("shut_down_p2pool_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I re-start p2pool node {}")]
#[when(expr = "I re-start p2pool node {}")]
async fn restart_p2pool_node(world: &mut TariWorld, p2pool_name: String) {
    if let Err(err) = restart_node(world, p2pool_name).await {
        let msg = format!("restart_p2pool_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}
