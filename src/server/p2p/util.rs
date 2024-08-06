// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use libp2p::identity::Keypair;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GenerateIdentityResult {
    peer_id: PeerId,
    private_key: Vec<u8>,
}

impl GenerateIdentityResult {
    pub fn new(peer_id: PeerId, private_key: Vec<u8>) -> Self {
        Self {
            peer_id,
            private_key,
        }
    }
}

pub async fn generate_identity() -> anyhow::Result<GenerateIdentityResult> {
    let key_pair = Keypair::generate_ed25519();
    let encoded_private_key = key_pair.to_protobuf_encoding()?;

    Ok(GenerateIdentityResult::new(
        key_pair.public().to_peer_id(),
        encoded_private_key,
    ))
}