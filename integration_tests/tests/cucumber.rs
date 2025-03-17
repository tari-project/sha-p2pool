// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause
#![feature(internal_output_capture)]

use std::{
    fs,
    io,
    path::PathBuf,
    process::Command,
    str::{self},
    sync::{Arc, Mutex},
};

use cucumber::{event::ScenarioFinished, writer, writer::Verbosity, World as _};
use integration_tests::{p2pool_process::get_p2pool_exe_path, TariWorld};
use log::*;
use tari_common::{configuration::Network, initialize_logging, network_check::set_network_if_choice_valid};
use tokio::runtime::Runtime;
pub mod steps;

pub const LOG_TARGET: &str = "cucumber";
pub const LOG_TARGET_STDOUT: &str = "stdout";

fn flush_stdout(buffer: &Arc<Mutex<Vec<u8>>>) {
    // After each test we flush the stdout to the logs.
    info!(target: LOG_TARGET_STDOUT, "{}", str::from_utf8(&buffer.lock().unwrap()).unwrap());
    buffer.lock().unwrap().clear();
}

#[allow(clippy::too_many_lines)]
fn main() {
    std::env::set_var("TARI_TARGET_NETWORK", "localnet");
    std::env::set_var("TARI_NETWORK", "localnet");
    std::env::set_var("PERFORM_DETAIL_LOGGING", "true");
    if let Err(err) = set_network_if_choice_valid(Network::LocalNet) {
        let msg = format!("Error setting network: {}", err);
        error!(target: LOG_TARGET, "{}", msg);
        panic!("{}", msg);
    }

    let p2pool_exe = get_p2pool_exe_path();
    let rebuild = match fs::exists(&p2pool_exe) {
        Ok(exists) => {
            if exists {
                println!("Found sha_p2pool executable at: '{}'", p2pool_exe.display());
                match std::env::var("DO_NOT_REBUILD_SHA_P2POOL") {
                    Ok(val) => {
                        println!(
                            "Secondary build environment variable: 'DO_NOT_REBUILD_SHA_P2POOL = {}'",
                            val
                        );
                        val == "0" || val.to_uppercase() == "false".to_uppercase()
                    },
                    Err(_) => true,
                }
            } else {
                println!("sha_p2pool executable not found at: '{}'", p2pool_exe.display());
                true
            }
        },
        Err(err) => {
            panic!(
                "Failed to check if sha_p2pool executable exists at: '{}', error: {}",
                p2pool_exe.display(),
                err
            );
        },
    };
    if rebuild {
        println!("Building sha_p2pool executable in release mode...");
        let output = Command::new("cargo")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))
            .env("TARI_TARGET_NETWORK", "localnet")
            .env("PERFORM_DETAIL_LOGGING", "true")
            .arg("build")
            .arg("--release")
            .arg("--bin")
            .arg("sha_p2pool")
            .output()
            .expect("Failed to build sha_p2pool in release mode");
        if !output.status.success() {
            panic!(
                "Failed to build sha_p2pool in release mode: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    initialize_logging(
        &PathBuf::from("log4rs/cucumber.yml"),
        &PathBuf::from("./"),
        include_str!("../log4rs/cucumber.yml"),
    )
    .expect("logging not configured");
    let stdout_buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    #[cfg(test)]
    std::io::set_output_capture(Some(stdout_buffer.clone()));

    // IMPORTANT: Never move this line below the runtime creation as it will prevent capture in task::spawn threads
    let runtime = Runtime::new().unwrap();
    runtime.block_on(async {
        let world = TariWorld::cucumber()
            .repeat_failed()
            // 'max_concurrent_scenarios' must be == 1 for mDNS to work properly, as mDNS has global state and can't 
            // handle concurrent tests 
            .max_concurrent_scenarios(1)
            .after(move |_feature, _rule, scenario, ev, maybe_world| {
                Box::pin(async move {
                    match ev {
                        ScenarioFinished::StepFailed(_capture_locations, _location, _error) => {
                            error!(target: LOG_TARGET, "Scenario failed");
                        },
                        ScenarioFinished::StepPassed => {
                            info!(target: LOG_TARGET, "Scenario was successful.");
                        },
                        ScenarioFinished::StepSkipped => {
                            warn!(target: LOG_TARGET, "Some steps were skipped.");
                        },
                        ScenarioFinished::BeforeHookFailed(_info) => {
                            error!(target: LOG_TARGET, "Before hook failed!");
                        },
                    }
                    if let Some(maybe_world) = maybe_world {
                        maybe_world.after(scenario).await;
                    }
                })
            })
            .before(move |feature, _rule, scenario, world| {
                Box::pin(async move {
                    println!("{} : {}", scenario.keyword, scenario.name); // This will be printed into the stdout_buffer
                    info!(target: LOG_TARGET, "Starting {} {}", scenario.keyword, scenario.name);

                    world.before(feature, scenario).await;
                })
            });
        let file = fs::File::create("cucumber-output-junit.xml")
            .unwrap_or_else(|e| panic!("Failed to create output file 'cucumber-output-junit.xml': {}", e));
        world
            .fail_on_skipped()
            // .fail_fast() - Not yet supported in 0.18
            .with_writer(writer::Tee::new(writer::JUnit::new(file, Verbosity::ShowWorldAndDocString),
                                          writer::Summarize::new(writer::Basic::new(io::stdout(), writer::Coloring::Auto, Verbosity::ShowWorldAndDocString))))
            .run("tests/features/")
            .await;
    });

    // If by any chance we have anything in the stdout buffer just log it.
    flush_stdout(&stdout_buffer);
}
