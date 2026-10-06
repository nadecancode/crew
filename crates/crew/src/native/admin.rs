use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::{json, Value};

pub fn admin_call(method: &str, params: Value) -> Result<Value> {
    crew_core::client::refuse_in_agent_context()?;
    let socket = crew_core::paths::runtime_dir().join("native.sock");
    let token = crew_core::paths::read_operator_token(&socket)?
        .ok_or_else(|| anyhow::anyhow!("native control credential unavailable"))?;
    let mut stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = |id, method: &str, params: Value| -> Result<Value> {
        let mut bytes = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))?;
        if bytes.len() > 64 * 1024 {
            bail!("native control request exceeds its limit");
        }
        bytes.push(b'\n');
        stream.write_all(&bytes)?;
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)?;
        if bytes.len() > 1024 * 1024 || !bytes.ends_with(b"\n") {
            bail!("native control response unavailable");
        }
        let frame: crew_core::rpc::ServerFrame = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid native control response"))?;
        if frame.id != Some(id) {
            bail!("invalid native control response identity");
        }
        if let Some(error) = frame.error {
            bail!("native control: {}", error.code);
        }
        frame
            .result
            .ok_or_else(|| anyhow::anyhow!("native control response unavailable"))
    };
    request(
        1,
        "hello",
        json!({"role":"operator","client":"native-cli","protocol":6,"token":token}),
    )?;
    request(2, method, params)
}
