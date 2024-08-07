// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use libp2p::identity::Keypair;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GenerateIdentityResult {
    private_key: Vec<u8>,
}

impl GenerateIdentityResult {
    pub fn new(private_key: Vec<u8>) -> Self {
        Self { private_key }
    }

    pub fn private_key(&self) -> &Vec<u8> {
        &self.private_key
    }
}

/// Generates a new private key that can be used to return when generating identity.
pub async fn generate_identity() -> anyhow::Result<GenerateIdentityResult> {
    Ok(GenerateIdentityResult::new(
        Keypair::generate_ed25519().to_protobuf_encoding()?,
    ))
}
