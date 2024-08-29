// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use axum::http::StatusCode;

pub async fn handle_health() -> Result<StatusCode, StatusCode> {
    Ok(StatusCode::OK)
}
