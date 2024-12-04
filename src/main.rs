use std::{fs::File, io::Write, panic};

// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause
use clap::Parser;
use log::error;
use tari_shutdown::Shutdown;

use crate::cli::Cli;

mod cli;
mod server;
mod sharechain;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Set a custom panic hook
    panic::set_hook(Box::new(|panic_info| {
        let location = panic_info
            .location()
            .map(|loc| format!("{} file: '{}', line: {}", SystemTime::now(), loc.file(), loc.line()))

            .unwrap_or_else(|| "unknown location".to_string());

        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic message".to_string()
        };

        error!(target: "tari::p2pool::main", "Panic occurred at {}: {}", location, message);

        // Optionally, write a custom message directly to the file
        let mut file = File::create("panic.log").unwrap();
        file.write_all(format!("Panic at {}: {}", location, message).as_bytes())
            .unwrap();
        process::exit(500);
    }));

    Cli::parse().handle_command(Shutdown::new().to_signal()).await?;
    Ok(())
}
