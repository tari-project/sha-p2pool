// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use axum::http::StatusCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn handle_version() -> Result<String, StatusCode> {
    Ok(VERSION.to_string())
}
