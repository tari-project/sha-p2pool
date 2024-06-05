// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .include_file("tari.sha_p2pool.rs")
        .compile(
            &[
                "proto/p2pool.proto",
            ],
            &["proto", "submodules/tari/applications/minotari_app_grpc/proto/"],
        )?;

    Ok(())
}
