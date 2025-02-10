// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::Instant;

use log::*;
use minotari_app_grpc::tari_rpc::{
    pow_algo::PowAlgos,
    Block as GrpcBlock,
    GetNewBlockRequest,
    PowAlgo,
    SubmitBlockRequest,
};
use reqwest::Client;
use serde_json::Value;
use tari_common::configuration::Network;
use tari_common_types::tari_address::{TariAddress, TariAddressFeatures};
use tari_core::{
    blocks::{Block, BlockHeader},
    proof_of_work::{sha3x_difficulty, Difficulty},
};
use tari_crypto::{compressed_key::CompressedKey, ristretto::RistrettoPublicKey};
use tokio::time::{timeout, Duration};

use crate::{TariWorld, TestResult, HUNDRED_MS, THIRTY_SECONDS_WITH_100_MS_SLEEP};
pub const LOG_TARGET: &str = "cucumber::miner";

pub async fn mine_and_submit_tari_blocks(
    world: &mut TariWorld,
    number_of_blocks: u64,
    p2pool_name: String,
) -> TestResult<()> {
    debug!(target: LOG_TARGET, "add {} blocks to p2pool node '{}'", number_of_blocks, p2pool_name);

    let mut p2pool_client = world
        .get_p2pool_grpc_client(&p2pool_name)
        .await
        .map_err(|e| format!("Failed to get p2pool node grpc client: {}", e))?;

    for i in 0..number_of_blocks {
        let wallet_payment_address = new_random_dual_tari_address().to_hex();
        let block = timeout(Duration::from_secs(10), async {
            p2pool_client
                .get_new_block(GetNewBlockRequest {
                    pow: Some(PowAlgo {
                        pow_algo: PowAlgos::Sha3x as i32,
                    }),
                    coinbase_extra: i.to_string(),
                    wallet_payment_address: wallet_payment_address.clone(),
                })
                .await
        })
        .await
        .map_err(|e| format!("Timeout occurred: {}", e))?
        .map_err(|e| format!("Failed to get new block: {}", e))?;

        let block_response = block.into_inner();
        let target_difficulty = block_response.target_difficulty;
        let block_result = block_response.block.ok_or("Block response missing block")?;
        let grpc_block = block_result.block.ok_or("Block result missing block")?;
        let mut block = Block::try_from(grpc_block).map_err(|e| format!("Failed to convert gRPC block: {}", e))?;

        debug!(target: LOG_TARGET, "mining block '{}' with target difficulty '{}' (?'{}')", i, Difficulty::min(), target_difficulty);
        find_sha3x_header_with_achieved_difficulty(&mut block.header, Difficulty::min())?;

        timeout(Duration::from_secs(10), async {
            p2pool_client
                .submit_block(SubmitBlockRequest {
                    block: Some(
                        GrpcBlock::try_from(block).map_err(|e| format!("Failed to convert block to gRPC: {}", e))?,
                    ),
                    wallet_payment_address,
                })
                .await
                .map_err(|e| format!("Failed to submit block: {}", e))
        })
        .await
        .map_err(|e| format!("Timeout occurred: {}", e))??;
    }

    debug!(target: LOG_TARGET, "{} blocks added to p2pool node '{}'", number_of_blocks, p2pool_name);
    Ok(())
}

pub fn new_random_dual_tari_address() -> TariAddress {
    let mut rng = rand::thread_rng();
    let (_, view) = CompressedKey::<RistrettoPublicKey>::random_keypair(&mut rng);
    let (_, spend) = CompressedKey::<RistrettoPublicKey>::random_keypair(&mut rng);
    TariAddress::new_dual_address(
        view,
        spend,
        Network::LocalNet,
        TariAddressFeatures::create_interactive_and_one_sided(),
    )
}

pub fn find_sha3x_header_with_achieved_difficulty(
    header: &mut BlockHeader,
    achieved_difficulty: Difficulty,
) -> TestResult<()> {
    let mut num_tries = 0;

    while sha3x_difficulty(header).map_err(|e| format!("Failed to calculate difficulty: {}", e))? != achieved_difficulty
    {
        header.nonce += 1;
        num_tries += 1;
        if num_tries > 1_000_000 {
            // Just in case we burn a hole in the CI server
            return Err(format!(
                "Could not find a nonce for achieved difficulty {} in time",
                achieved_difficulty
            )
            .into());
        }
    }
    Ok(())
}

pub async fn verify_block_height(world: &mut TariWorld, p2pool_name: String, height: u64) -> TestResult<()> {
    debug!(target: LOG_TARGET, "verify '{}' is at height {}", p2pool_name, height);
    let start = Instant::now();

    let p2pool_process = world.get_p2pool_node(&p2pool_name)?;
    if !p2pool_process.config.http_server.enabled {
        return Err(format!("p2pool node '{}' doesn't have the http server enabled", p2pool_name).into());
    }
    let stats_url = format!("http://127.0.0.1:{}/stats", p2pool_process.config.http_server.port);
    let p2pool_client = Client::new();

    let mut local_height = 0;
    let mut stats: Value = Value::Null;
    for i in 0..(THIRTY_SECONDS_WITH_100_MS_SLEEP) {
        let response = timeout(Duration::from_secs(10), async {
            p2pool_client.get(stats_url.clone()).send().await
        })
        .await
        .map_err(|e| format!("Timeout occurred: {}", e))?
        .map_err(|e| format!("Failed to send request: {}", e))?;

        if response.status().is_success() {
            stats = response.json().await?;
            local_height = stats["sha3x_stats"]["height"].as_u64().ok_or("'height' not found")? + 1;
            match local_height.cmp(&height) {
                std::cmp::Ordering::Equal => {
                    debug!(
                        target: LOG_TARGET,
                        "Node '{}' is at height '{}' (waited {:.2?})",
                        p2pool_name, local_height, start.elapsed()
                    );
                    return Ok(());
                },
                std::cmp::Ordering::Greater => {
                    return Err(format!(
                        "Node '{}' busted its height - should be at '{}', now at '{}' (waited {:.2?})",
                        p2pool_name,
                        height,
                        local_height,
                        start.elapsed()
                    )
                    .into());
                },
                std::cmp::Ordering::Less => {
                    if i % 10 == 0 {
                        debug!(
                            target: LOG_TARGET,
                            "{}: '{}' is at height {}, need to be at {}",
                            i, p2pool_name, local_height, height
                        );
                    }
                },
            }
        } else {
            return Err(format!("Failed to query {} for stats: {}", stats_url, response.status()).into());
        }

        tokio::time::sleep(Duration::from_millis(HUNDRED_MS)).await;
    }

    error!(target: LOG_TARGET, "Height not achieved. Stats: {:?}", stats);
    Err(format!(
        "p2pool node '{}' didn't synchronize successfully at height {}, current chain height {}",
        p2pool_name, height, local_height
    )
    .into())
}
