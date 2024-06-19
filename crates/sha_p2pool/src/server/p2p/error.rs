use libp2p::{multiaddr, noise, TransportError};
use libp2p::gossipsub::PublishError;
use thiserror::Error;

use crate::server::p2p;
use crate::sharechain;

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
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError<std::io::Error>),
    #[error("I/O error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Behaviour error: {0}")]
    Behaviour(String),
    #[error("Gossip sub publish error: {0}")]
    Publish(#[from] PublishError),
}