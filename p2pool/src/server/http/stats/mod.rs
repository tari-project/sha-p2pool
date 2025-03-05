// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

pub mod handlers;
pub mod models;

pub(crate) const MAX_ACCEPTABLE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
