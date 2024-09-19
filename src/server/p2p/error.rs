// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::string::FromUtf8Error;

use libp2p::{
    gossipsub::PublishError,
    identity::DecodingError,
    kad::NoKnownPeers,
    multiaddr,
    noise,
    swarm::DialError,
    TransportError,
};
use thiserror::Error;

use crate::{server::p2p, sharechain};

#[derive(Error, Debug)]
pub enum Error {
    #[error("LibP2P error: {0}")]
    LibP2P(#[from] LibP2PError),
    #[error("CBOR serialize/deserialize error: {0}")]
    SerializeDeserialize(#[from] serde_cbor::Error),
    #[error("Share chain error: {0}")]
    ShareChain(#[from] sharechain::error::Error),
    #[error("Share chain error: {0}")]
    Client(#[from] p2p::client::ClientError),
}

#[derive(Error, Debug)]
pub enum LibP2PError {
    #[error("Noise error: {0}")]
    Noise(#[from] noise::Error),
    #[error("Multi address parse error: {0}")]
    MultiAddrParse(#[from] multiaddr::Error),
    #[error("Multi address empty")]
    MultiAddrEmpty,
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError<std::io::Error>),
    #[error("I/O error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Behaviour error: {0}")]
    Behaviour(String),
    #[error("Gossip sub publish error: {0}")]
    Publish(#[from] PublishError),
    #[error("Dial error: {0}")]
    Dial(#[from] DialError),
    #[error("Kademlia: No known peers error: {0}")]
    KademliaNoKnownPeers(#[from] NoKnownPeers),
    #[error("Missing peer ID from address: {0}")]
    MissingPeerId(String),
    #[error("Key decode error: {0}")]
    KeyDecoding(#[from] DecodingError),
    #[error("Invalid DNS entry: {0}")]
    InvalidDnsEntry(String),
    #[error("Failed to convert bytes to string: {0}")]
    ConvertBytesToString(#[from] FromUtf8Error),
}
