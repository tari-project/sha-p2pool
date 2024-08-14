// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use crate::server::p2p;

pub async fn handle_generate_identity() -> anyhow::Result<()> {
    let result = p2p::util::generate_identity().await?;
    print!("{}", serde_json::to_value(result)?);
    Ok(())
}
