//! Endpoint and connection handling for frontends and the runtime.
use crate::protocol::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub protocol: u32,
    pub installation_build: String,
    pub control: SocketAddr,
    pub data: SocketAddr,
    pub secret: String,
    pub pid: u32,
}
pub fn write_endpoint(state: &Path, endpoint: &Endpoint) -> io::Result<()> {
    let dir = state.join("service");
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!("endpoint-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, serde_json::to_vec(endpoint)?)?;
    let result = std::fs::rename(&temp, dir.join("endpoint.json"));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
pub fn read_endpoint(state: &Path) -> io::Result<Endpoint> {
    serde_json::from_slice(&std::fs::read(state.join("service/endpoint.json"))?)
        .map_err(io::Error::other)
}
pub fn send<T: Serialize>(stream: &mut TcpStream, value: &T, max: u32) -> io::Result<()> {
    write_frame(stream, &serde_json::to_vec(value)?, max)
}
pub fn receive<T: DeserializeOwned>(stream: &mut TcpStream, max: u32) -> io::Result<T> {
    serde_json::from_slice(&read_frame(stream, max)?).map_err(io::Error::other)
}
pub fn connect(endpoint: &Endpoint, address: SocketAddr, build: &str) -> io::Result<TcpStream> {
    if !address.ip().is_loopback() || endpoint.protocol != PROTOCOL_VERSION {
        return Err(io::Error::other("invalid endpoint"));
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    send(
        &mut stream,
        &Hello {
            protocol: PROTOCOL_VERSION,
            installation_build: build.to_owned(),
            secret: endpoint.secret.clone(),
        },
        MAX_CONTROL_FRAME_BYTES,
    )?;
    let reply: HelloReply = receive(&mut stream, MAX_CONTROL_REPLY_BYTES)?;
    if reply != HelloReply::Accepted {
        return Err(io::Error::new(
            if reply == HelloReply::Stopping {
                io::ErrorKind::ConnectionAborted
            } else {
                io::ErrorKind::PermissionDenied
            },
            format!("hello rejected: {reply:?}"),
        ));
    }
    Ok(stream)
}
