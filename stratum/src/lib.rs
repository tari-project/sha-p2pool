use log::info;
use tari_shutdown::ShutdownSignal;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    select,
};

const LOG_TARGET: &str = "tari::stratum";

pub struct StratumServerBuilder<T> {
    port: Option<u16>,
    with_job_handler: Option<T>,
}

impl<T: StratumJobHandler> StratumServerBuilder<T> {
    pub fn new() -> Self {
        StratumServerBuilder {
            port: None,
            with_job_handler: None,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn with_job_handler(mut self, handler: T) -> Self {
        self.with_job_handler = Some(handler);
        self
    }

    pub fn build(self) -> StratumServer<T> {
        StratumServer {
            // Set default port if not provided
            port: self.port.unwrap_or(3333),
            hander: self.with_job_handler.expect("Job handler must be provided"),
        }
    }
}

pub struct StratumServer<T: StratumJobHandler> {
    port: u16,
    hander: T,
}

impl<T: StratumJobHandler> StratumServer<T> {
    pub async fn start(&self, mut shutdown_signal: ShutdownSignal) -> anyhow::Result<()> {
        info!(target: LOG_TARGET, "Starting Stratum server on port {}", self.port);
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        loop {
            select! {
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET, "Shutting down Stratum server");
                    break;
                },
                // Handle incoming connections and jobs here
                res = listener.accept() => {
                    match res {
                        Ok((stream, _)) => {
                            // Handle the connection with the job handler
                            info!(target: LOG_TARGET, "Accepted connection from {}", stream.peer_addr()?);
                            // self.hander.handle_connection(stream).await?;
                            tokio::spawn(async move {
                                let (reader, mut writer) = stream.into_split();
                                let mut reader = BufReader::new(reader).lines();

                                while let Ok(Some(line)) = reader.next_line().await {
                                    // if let Ok(msg): Result<Value, _> = serde_json::from_str(&line) {
                                        // handle 'login', 'submit', etc.
                                        println!("Received: {:#?}", line);
                                    // }
                                }
                            });
                        }
                        Err(e) => {
                            info!(target: LOG_TARGET, "Failed to accept connection: {}", e);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub trait StratumJobHandler {}

pub trait StratumStreamAdapter {}
