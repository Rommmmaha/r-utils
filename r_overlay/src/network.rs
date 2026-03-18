use crate::draw::DrawOperation;
use anyhow::Result;
use crossbeam::channel::Sender;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
#[derive(Deserialize)]
pub struct Command {
    pub layer: Option<i32>,
    pub timeout_ms: Option<u64>,
    pub operations: Vec<DrawOperation>,
}
pub async fn start_listeners(
    port: Option<u16>,
    unix_path: Option<&str>,
    sender: Sender<Command>,
) -> Result<()> {
    let mut handles = vec![];
    if let Some(port) = port {
        let sender = sender.clone();
        log::info!("Starting UDP listener on port {}", port);
        let handle = tokio::spawn(async move {
            let socket = tokio::net::UdpSocket::bind(("0.0.0.0", port)).await?;
            log::info!("UDP socket bound successfully");
            let mut buf = [0; 65536];
            loop {
                let (len, addr) = socket.recv_from(&mut buf).await?;
                log::info!("Received {} bytes from {}", len, addr);
                let json = std::str::from_utf8(&buf[..len])?;
                log::debug!("JSON: {}", json);
                match serde_json::from_str::<Command>(json) {
                    Ok(cmd) => {
                        log::info!("Parsed command successfully, sending to main thread");
                        if let Err(e) = sender.send(cmd) {
                            log::error!("Failed to send command: {}", e);
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to parse JSON: {}", e);
                    }
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
        handles.push(handle);
    }
    if let Some(path) = unix_path {
        let sender = sender.clone();
        if std::path::Path::new(path).exists() {
            let _ = std::fs::remove_file(path);
            log::info!("Removed existing socket at {}", path);
        }
        let path = path.to_string();
        log::info!("Starting Unix socket listener at {}", path);
        let handle = tokio::spawn(async move {
            let listener = tokio::net::UnixListener::bind(&path)?;
            log::info!("Unix socket bound successfully at {}", path);
            loop {
                let (mut stream, _) = listener.accept().await?;
                log::info!("Unix socket connection accepted");
                let sender = sender.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    stream.read_to_end(&mut buf).await?;
                    log::info!("Read {} bytes from Unix socket", buf.len());
                    let json = std::str::from_utf8(&buf)?;
                    log::debug!("JSON: {}", json);
                    match serde_json::from_str::<Command>(json) {
                        Ok(cmd) => {
                            log::info!("Parsed command successfully, sending to main thread");
                            if let Err(e) = sender.send(cmd) {
                                log::error!("Failed to send command: {}", e);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to parse JSON: {}", e);
                        }
                    }
                    Ok::<(), anyhow::Error>(())
                });
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
        handles.push(handle);
    }
    for handle in handles {
        let _ = handle.await?;
    }
    Ok(())
}
