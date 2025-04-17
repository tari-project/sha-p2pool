// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::VecDeque,
    fmt::{Debug, Formatter},
    path::PathBuf,
};

use cucumber::{
    codegen::anyhow,
    gherkin::{Feature, Scenario},
};
use indexmap::IndexMap;
use log::error;
use minotari_app_grpc::tari_rpc::sha_p2_pool_client::ShaP2PoolClient;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tari_core::blocks::Block;
use thiserror::Error;
use tonic::transport::Channel as TonicChannel;

use crate::{base_node_process::BaseNodeProcess, get_base_dir, P2PoolProcess};

pub const LOG_TARGET: &str = "cucumber::world";

#[derive(Error, Debug)]
pub enum TariWorldError {
    #[error("P2Pool process not found: {0}")]
    P2PoolProcessNotFound(String),
    #[error("Base node process not found: {0}")]
    BaseNodeProcessNotFound(String),
}

#[derive(cucumber::World, Default)]
pub struct TariWorld {
    pub current_scenario_name: Option<String>,
    pub current_feature_name: Option<String>,
    pub current_base_dir: Option<PathBuf>,
    pub p2pool_nodes: IndexMap<String, P2PoolProcess>,
    pub base_nodes: IndexMap<String, BaseNodeProcess>,
    pub seed_nodes: Vec<String>,
    pub blocks: IndexMap<String, Block>,
    pub errors: VecDeque<String>,
}

impl Debug for TariWorld {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("p2pool_nodes", &self.p2pool_nodes)
            .field("base_nodes", &self.base_nodes)
            .field("seed_nodes", &self.seed_nodes)
            .field("blocks", &self.blocks)
            .field("errors", &self.errors)
            .finish()
    }
}

impl TariWorld {
    pub async fn get_p2pool_grpc_client<S: AsRef<str> + std::fmt::Display>(
        &mut self,
        name: &S,
    ) -> anyhow::Result<ShaP2PoolClient<TonicChannel>> {
        self.get_p2pool_node(name)
            .inspect_err(|e| error!(target: LOG_TARGET, "p2pool node '{}' not found: {}", name, e))?
            .get_grpc_client()
            .await
            .inspect_err(|e| error!(target: LOG_TARGET, "Could not connect p2pool node '{}' grpc client: {}", name, e))
    }

    pub async fn get_base_node_client<S: AsRef<str> + std::fmt::Display>(
        &self,
        name: &S,
    ) -> anyhow::Result<BaseNodeGrpcClient<TonicChannel>> {
        self.get_base_node(name)
            .inspect_err(|e| error!(target: LOG_TARGET, "base node '{}' not found: {}", name, e))?
            .get_grpc_client()
            .await
            .inspect_err(|e| error!(target: LOG_TARGET, "Could not connect base node '{}' grpc client: {}", name, e))
    }

    pub fn get_p2pool_node<S: AsRef<str>>(&mut self, node_name: &S) -> anyhow::Result<&mut P2PoolProcess> {
        Ok(self
            .p2pool_nodes
            .get_mut(node_name.as_ref())
            .ok_or_else(|| TariWorldError::P2PoolProcessNotFound(node_name.as_ref().to_string()))?)
    }

    pub fn get_base_node<S: AsRef<str>>(&self, node_name: &S) -> anyhow::Result<&BaseNodeProcess> {
        Ok(self
            .base_nodes
            .get(node_name.as_ref())
            .ok_or_else(|| TariWorldError::BaseNodeProcessNotFound(node_name.as_ref().to_string()))?)
    }

    pub fn all_seed_nodes(&self) -> &[String] {
        self.seed_nodes.as_slice()
    }

    pub async fn before(&mut self, feature: &Feature, scenario: &Scenario) {
        self.current_feature_name = Some(feature.name.clone());
        self.current_scenario_name = Some(scenario.name.clone());
        let mut base_dir = get_base_dir().join(feature.name.clone()).join(scenario.name.clone());
        base_dir = PathBuf::from(base_dir.to_string_lossy().replace(" ", "_"));
        self.current_base_dir = Some(base_dir);
    }

    pub async fn after(&mut self, _scenario: &Scenario) {
        for (name, mut p) in self.p2pool_nodes.drain(..) {
            println!("Shutting down p2pool node {}", name);
            p.kill();
        }
        for (name, mut p) in self.base_nodes.drain(..) {
            println!("Shutting down base node {}", name);
            // You have explicitly trigger the shutdown now because of the change to use Arc/Mutex in tari_shutdown
            p.kill_signal.trigger();
        }
    }
}
