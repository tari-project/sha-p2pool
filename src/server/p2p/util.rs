// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use libp2p::{identity::Keypair, PeerId};
use serde::{Deserialize, Serialize};
use tari_utilities::hex::Hex;

#[derive(Serialize, Deserialize)]
pub struct GenerateIdentityResult {
    peer_id: PeerId,
    private_key: String,
}

impl GenerateIdentityResult {
    pub fn new(peer_id: PeerId, private_key: String) -> Self {
        Self { peer_id, private_key }
    }
}

/// Generates a new identity (private key for libp2p).
pub async fn generate_identity() -> anyhow::Result<GenerateIdentityResult> {
    let private_key = Keypair::generate_ed25519();
    let private_key_raw = private_key.to_protobuf_encoding()?.to_hex();
    Ok(GenerateIdentityResult::new(
        private_key.public().to_peer_id(),
        private_key_raw,
    ))
}
