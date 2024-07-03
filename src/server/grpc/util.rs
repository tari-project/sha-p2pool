// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::Duration;

use log::error;
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tokio::time::sleep;
use tonic::transport::Channel;

use crate::server::grpc::error::{Error, TonicError};

/// Utility function to connect to a Base node and try infinitely when it fails until gets connected.
pub async fn connect_base_node(base_node_address: String) -> Result<BaseNodeClient<Channel>, Error> {
    let client_result = BaseNodeGrpcClient::connect(base_node_address.clone())
        .await
        .map_err(|e| Error::Tonic(TonicError::Transport(e)));
    let client = match client_result {
        Ok(client) => client,
        Err(error) => {
            error!("[Retry] Failed to connect to Tari base node: {:?}", error.to_string());
            let mut client = None;
            while client.is_none() {
                sleep(Duration::from_secs(5)).await;
                match BaseNodeGrpcClient::connect(base_node_address.clone())
                    .await
                    .map_err(|e| Error::Tonic(TonicError::Transport(e)))
                {
                    Ok(curr_client) => client = Some(curr_client),
                    Err(error) => error!("[Retry] Failed to connect to Tari base node: {:?}", error.to_string()),
                }
            }
            client.unwrap()
        },
    };

    Ok(client)
}
