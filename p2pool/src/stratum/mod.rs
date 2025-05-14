use tari_stratum::{StratumJobHandler, StratumStreamAdapter};

pub struct StratumJobHandlerImpl {}

impl StratumJobHandlerImpl {
    pub fn new() -> Self {
        StratumJobHandlerImpl {}
    }
}

impl StratumJobHandler for StratumJobHandlerImpl {}

pub struct StratumStreamAdapterImpl {}

impl StratumStreamAdapter for StratumStreamAdapterImpl {}
