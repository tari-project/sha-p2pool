// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    fs::File,
    io::Write,
    panic,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::signal;
use clap::Parser;
use log::error;
use sha_p2pool::Cli;
use tari_shutdown::Shutdown;
use ctrlc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn format_system_time(time: SystemTime) -> String {
    let datetime = time.duration_since(UNIX_EPOCH).unwrap();
    let seconds = datetime.as_secs();
    let nanos = datetime.subsec_nanos();
    let naive = chrono::DateTime::from_timestamp(seconds.try_into().unwrap(), nanos).unwrap();
    naive.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "dhat-heap")]
    let dhat_profiler = dhat::Profiler::new_heap();
    #[cfg(feature = "dhat-heap")]
    println!("\n\nDHAT: Profiling enabled. Run `dhat-heap` to view the results.\n\n");

    // Set up the Ctrl-C handler (needed to enable manual drop of the dhat_profiler)
    let should_exit = Arc::new(AtomicBool::new(false));
    let should_exit_clone = Arc::clone(&should_exit);
    ctrlc::set_handler(move || {
        should_exit_clone.store(true, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    // Set a custom panic hook
    panic::set_hook(Box::new(|panic_info| {
        let location = panic_info
            .location()
            .map(|loc| {
                format!(
                    "{} file: '{}', line: {}",
                    format_system_time(SystemTime::now()),
                    loc.file(),
                    loc.line()
                )
            })
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
        if cfg!(debug_assertions) {
            // In debug mode, we want to see the panic message
            eprintln!("Panic occurred at {}: {}", location, message);
            std::process::exit(500);
        }
    }));

    let binding = Cli::parse();
    let command_future = binding.handle_command(Shutdown::new());

    // while !should_exit.load(Ordering::SeqCst) {
    //     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    // }

    tokio::select! {
        _ = async move {
            let _unused = command_future.await;
        } => {
            // Command future completed
        },
        _ = signal::ctrl_c() => {
            should_exit.store(true, Ordering::SeqCst);
        },
    }


    if should_exit.load(Ordering::SeqCst) {
        println!("\nCtrl-C pressed, exiting...\n");
    }

    #[cfg(feature = "dhat-heap")]
    drop(dhat_profiler);

    Ok(())
}
