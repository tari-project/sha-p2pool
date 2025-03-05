// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use cucumber::{given, when};
use log::*;
use tari_integration_tests::{base_node_process::spawn_base_node, TariWorld};
use tokio::time::Duration;

pub const LOG_TARGET: &str = "cucumber::base_node_steps";

#[given(expr = "I have a base node seed {word}")]
#[when(expr = "I have a base node seed {word}")]
async fn start_seed_base_node(world: &mut TariWorld, name: String) {
    if let Err(err) = spawn_base_node(world, true, name.clone(), vec![]).await {
        let msg = format!("start_seed_base_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    debug!(target: LOG_TARGET, "start_seed_base_node: spawned '{}'", name);
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I have a base node {word}")]
#[when(expr = "I have a base node {word}")]
async fn start_base_node(world: &mut TariWorld, name: String) {
    if let Err(err) = spawn_base_node(world, false, name.clone(), vec![]).await {
        let msg = format!("start_base_node: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    debug!(target: LOG_TARGET, "start_base_node: spawned '{}'", name);
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[given(expr = "I have a base node {word} connected to all seed nodes")]
#[when(expr = "I have a base node {word} connected to all seed nodes")]
async fn start_base_node_connected_to_all_seed_nodes(world: &mut TariWorld, name: String) {
    if let Err(err) = spawn_base_node(world, false, name.clone(), world.all_seed_nodes().to_vec()).await {
        let msg = format!("start_base_node_connected_to_all_seed_nodes: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }
    debug!(target: LOG_TARGET, "start_base_node_connected_to_all_seed_nodes: spawned '{}'", name);
    tokio::time::sleep(Duration::from_secs(1)).await;
}
