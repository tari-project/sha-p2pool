// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use axum::http::StatusCode;
use axum::Json;

use crate::server::http::stats::models::Stats;

pub async fn handle_get_stats() -> (StatusCode, Json<Stats>) {
    todo!()
}