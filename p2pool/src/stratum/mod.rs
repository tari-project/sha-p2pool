use tari_stratum::{StratumJobHandler, StratumStreamAdapter};

#[derive(Clone)]
pub struct StratumJobHandlerImpl {}

impl StratumJobHandlerImpl {
    pub fn new() -> Self {
        StratumJobHandlerImpl {}
    }
}

impl StratumJobHandler for StratumJobHandlerImpl {
    fn handle_request(&self, request: tari_stratum::StratumRequest) -> anyhow::Result<serde_json::Value> {
        todo!()
    }
}
