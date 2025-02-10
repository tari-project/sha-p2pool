// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use clap::builder::{styling::AnsiColor, Styles};

pub fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightYellow.on_default())
        .usage(AnsiColor::BrightYellow.on_default())
        .literal(AnsiColor::BrightGreen.on_default())
        .placeholder(AnsiColor::BrightCyan.on_default())
        .error(AnsiColor::BrightRed.on_default())
        .invalid(AnsiColor::BrightRed.on_default())
        .valid(AnsiColor::BrightGreen.on_default())
}

pub fn validate_squad(squad: &str) -> Result<String, String> {
    if squad.trim().is_empty() {
        return Err(String::from("squad must be set"));
    }

    Ok(String::from(squad))
}
