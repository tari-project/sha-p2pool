// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{num::TryFromIntError, time::Duration};

use log::error;
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tari_shutdown::ShutdownSignal;
use tokio::select;
use tonic::transport::Channel;

use crate::server::{
    grpc::error::{Error, TonicError},
    p2p::Squad,
};

/// Utility function to connect to a Base node and try infinitely when it fails until gets connected.
pub async fn connect_base_node(
    base_node_address: String,
    shutdown_signal: ShutdownSignal,
) -> Result<BaseNodeClient<Channel>, Error> {
    let client_result = BaseNodeGrpcClient::connect(base_node_address.clone())
        .await
        .map_err(|e| Error::Tonic(TonicError::Transport(e)));
    let client = match client_result {
        Ok(client) => client,
        Err(error) => {
            error!("[Retry] Failed to connect to Tari base node: {:?}", error.to_string());
            let mut client = None;
            let mut retry_interval = tokio::time::interval(Duration::from_secs(5));
            tokio::pin!(shutdown_signal);
            while client.is_none() {
                select! {
                    () = &mut shutdown_signal => {
                        return Err(Error::Shutdown);
                    }
                    _ = retry_interval.tick() => {
                        match BaseNodeGrpcClient::connect(base_node_address.clone())
                            .await
                            .map_err(|e| Error::Tonic(TonicError::Transport(e)))
                        {
                            Ok(curr_client) => client = Some(curr_client),
                            Err(error) => error!("[Retry] Failed to connect to Tari base node: {:?}", error.to_string()),
                        }
                    }
                }
            }
            client.unwrap()
        },
    };

    Ok(client)
}

pub fn convert_coinbase_extra(squad: Squad, custom_coinbase_extra: String) -> Result<Vec<u8>, TryFromIntError> {
    let type_length_value_marker = 0xFFu8;
    let squad_type_marker = 0x02u8;
    let custom_message_type_marker = 0x01u8;

    let mut current_squad = squad.as_string().into_bytes();
    let current_squad_len = u8::try_from(current_squad.len())?;
    let mut result = vec![type_length_value_marker, squad_type_marker, current_squad_len];
    result.append(&mut current_squad);

    let mut custom_coinbase_extra_bytes = custom_coinbase_extra.into_bytes();
    let custom_coinbase_extra_len = u8::try_from(custom_coinbase_extra_bytes.len())?;
    result.append(&mut vec![custom_message_type_marker, custom_coinbase_extra_len]);
    result.append(&mut custom_coinbase_extra_bytes);

    Ok(result)
}
