use libp2p::{kad, multiaddr, noise, TransportError};
use thiserror::Error;

use crate::sharechain;

#[derive(Error, Debug)]
pub enum Error {
    #[error("LibP2P error: {0}")]
    LibP2P(#[from] LibP2PError),
    #[error("CBOR serialize error: {0}")]
    Serialize(#[from] serde_cbor::Error),
    #[error("Share chain error: {0}")]
    ShareChain(#[from] sharechain::Error),
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
    #[error("Kademlia record store error: {0}")]
    KadRecord(#[from] kad::store::Error),
}