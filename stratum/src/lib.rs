use std::marker::PhantomData;

use log::info;
use serde_json::Value;
use tari_shutdown::ShutdownSignal;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    select,
};

const LOG_TARGET: &str = "tari::stratum";

pub struct StratumServerBuilder<T, TAdapter: StratumStreamAdapter> {
    port: Option<u16>,
    with_job_handler: Option<T>,
    _marker: PhantomData<TAdapter>,
}

impl<T: StratumJobHandler, TAdapter: StratumStreamAdapter> StratumServerBuilder<T, TAdapter> {
    pub fn new() -> Self {
        StratumServerBuilder {
            port: None,
            with_job_handler: None,
            _marker: PhantomData::default(),
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

    pub fn build(self) -> StratumServer<T, TAdapter> {
        StratumServer {
            // Set default port if not provided
            port: self.port.unwrap_or(3333),
            hander: self.with_job_handler.expect("Job handler must be provided"),
            adapter: Default::default(),
        }
    }
}

pub struct StratumServer<T: StratumJobHandler, TAdapter: StratumStreamAdapter> {
    port: u16,
    hander: T,
    adapter: PhantomData<TAdapter>,
}

impl<T: StratumJobHandler, TAdapter: StratumStreamAdapter> StratumServer<T, TAdapter> {
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
                            let handler = self.hander.clone();
                            // self.hander.handle_connection(stream).await?;
                            tokio::spawn(async move {
                                let (reader, mut writer) = stream.into_split();
                                let mut reader = BufReader::new(reader).lines();

                                while let Ok(Some(line)) = reader.next_line().await {
                                    // if let Ok(msg): Result<Value, _> = serde_json::from_str(&line) {
                                        // handle 'login', 'submit', etc.
                                        println!("Received: {:#?}", line);
                                        match TAdapter::try_convert(line) {
                                            Ok(request) => {
                                                let id = request.id().to_string();
                                                match handler.handle_request(request) {
                                                    Ok(resp) => {
                                                        info!(target: LOG_TARGET, "Handled request with id: {}", id);
                                                        let json_response = serde_json::to_string(&resp).unwrap();
                                                        writer.write_all(format!("{{\"id\": \"{}\", \"result\": {}, \"error\": null}}\n", id, json_response).as_bytes()).await.unwrap();
                                                    },
                                                    Err(e) => {
                                                        info!(target: LOG_TARGET, "Failed to handle request: {}", e);
                                                        writer.write_all(format!("{{\"id\": \"{}\", \"error\": \"Failed to handle request:{}\", \"result\": null}}\n", id, e.to_string()).as_bytes()).await.unwrap();
                                                    }
                                                }
                                            },
                                            Err(e) => {
                                                info!(target: LOG_TARGET, "Failed to parse request: {}", e);
                                            }
                                        }
                                    // }
                                }

                            });
                        },

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

pub trait StratumJobHandler: Clone + Send + Sync + 'static {
    fn handle_request(&self, request: StratumRequest) -> anyhow::Result<Value>;
}

pub trait StratumStreamAdapter {
    fn try_convert(line: String) -> anyhow::Result<StratumRequest>;
}

pub struct NiceHashStyleStatumStreamAdapter {}

impl StratumStreamAdapter for NiceHashStyleStatumStreamAdapter {
    fn try_convert(line: String) -> anyhow::Result<StratumRequest> {
        let json: serde_json::Value = serde_json::from_str(&line)?;
        let method = json["method"]
            .as_str()
            .ok_or(anyhow::anyhow!("Json missing method field"))?;
        let id = json["id"]
            .as_i64()
            .ok_or(anyhow::anyhow!("Invalid JSON. Json missing id field"))?
            .to_string();
        match method {
            "login" => {
                let params = json["params"]
                    .as_object()
                    .ok_or(anyhow::anyhow!("Invalid JSON.params missing"))?;
                let login = params["login"]
                    .as_str()
                    .ok_or(anyhow::anyhow!("Invalid JSON. login missing"))?
                    .to_string();
                let pass = params["pass"]
                    .as_str()
                    .ok_or(anyhow::anyhow!("Invalid JSON. pass missing"))?
                    .to_string();
                let agent = params["agent"]
                    .as_str()
                    .ok_or(anyhow::anyhow!("Invalid JSON. agent missing"))?
                    .to_string();

                Ok(StratumRequest::Login { id, login, pass, agent })
            },
            "submit" => {
                let params = json["params"].as_array().ok_or(anyhow::anyhow!("Invalid JSON"))?;
                let job_id = params[0].as_str().ok_or(anyhow::anyhow!("Invalid JSON"))?.to_string();
                let nonce = params[1].as_str().ok_or(anyhow::anyhow!("Invalid JSON"))?.to_string();
                Ok(StratumRequest::Submit { id, job_id, nonce })
            },
            _ => Err(anyhow::anyhow!("Unknown method")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StratumRequest {
    Login {
        id: String,
        login: String,
        pass: String,
        agent: String,
    },
    Submit {
        id: String,
        job_id: String,
        nonce: String,
    },
}

impl StratumRequest {
    pub fn id(&self) -> &str {
        match self {
            StratumRequest::Login { id, .. } => id.as_str(),
            StratumRequest::Submit { id, .. } => id.as_str(),
        }
    }
}
