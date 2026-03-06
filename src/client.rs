use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::protocol::{Request, Response};

pub fn send(socket_path: &Path, request: &Request) -> anyhow::Result<Response> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|_| anyhow::anyhow!("Session not running (cannot connect to socket)"))?;

    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut data = serde_json::to_vec(request)?;
    data.push(b'\n');
    stream.write_all(&data)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response_data = Vec::new();
    stream.read_to_end(&mut response_data)?;

    let response: Response = serde_json::from_slice(&response_data)
        .map_err(|e| anyhow::anyhow!("Invalid response from daemon: {}", e))?;
    Ok(response)
}
