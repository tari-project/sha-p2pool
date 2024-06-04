use std::time::Duration;

/// Config is the server configuration struct.
#[derive(Clone)]
pub struct Config {
    pub base_node_address: String,
    pub p2p_port: u64,
    pub grpc_port: u64,
    pub idle_connection_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_node_address: String::from("http://127.0.0.1:18142"),
            p2p_port: 0, // bind to any free port
            grpc_port: 18143, // default local base node port + 1
            idle_connection_timeout: Duration::from_secs(30),
        }
    }
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder {
            config: Config::default(),
        }
    }
}

pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    pub fn with_p2p_port(&mut self, port: u64) -> &mut Self {
        self.config.p2p_port = port;
        self
    }

    pub fn with_grpc_port(&mut self, port: u64) -> &mut Self {
        self.config.grpc_port = port;
        self
    }

    pub fn with_idle_connection_timeout(&mut self, timeout: Duration) -> &Self {
        self.config.idle_connection_timeout = timeout;
        self
    }

    pub fn build(&self) -> Config {
        self.config.clone()
    }
}
