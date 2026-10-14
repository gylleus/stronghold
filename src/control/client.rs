//! Client side of the control socket, and the `stronghold ctl` debugging subcommand.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::control::protocol::{Request, Response};

/// How long the CLI waits for a response; a close can take up to three minutes.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(200);

/// A connection to the control socket; send any number of requests over it.
pub struct ControlClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl ControlClient {
    pub fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
        Ok(Self {
            writer: stream.try_clone()?,
            reader: BufReader::new(stream),
        })
    }

    /// Sends one raw line and reads one response line.
    pub fn call_raw(&mut self, line: &str) -> io::Result<String> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        let mut response = String::new();
        if self.reader.read_line(&mut response)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection",
            ));
        }
        Ok(response)
    }

    pub fn call(&mut self, request: &Request) -> io::Result<Response> {
        let line = serde_json::to_string(request).map_err(io::Error::other)?;
        let response = self.call_raw(&line)?;
        serde_json::from_str(&response).map_err(io::Error::other)
    }
}

/// `stronghold ctl <method> [params]`: prints the response; `None` for other subcommands.
pub fn run_cli(args: &[String]) -> Option<Result<()>> {
    let (cmd, rest) = args.split_first()?;
    (cmd == "ctl").then(|| ctl(rest))
}

fn ctl(args: &[String]) -> Result<()> {
    let Some((method, rest)) = args.split_first() else {
        bail!(
            "usage: stronghold ctl <method> [json params | id]\n\
             e.g. stronghold ctl status; stronghold ctl agent term:abc; \
             stronghold ctl agents '{{\"state\":\"needs_input\"}}'"
        );
    };
    let params = cli_params(rest)?;
    let path = crate::control::socket_path();
    let mut client = ControlClient::connect(&path)
        .with_context(|| format!("connecting to {} (is Stronghold running?)", path.display()))?;
    let response = client.call(&Request::new(method.as_str(), params))?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    if !response.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Params from the arguments after the method: a JSON object, or a bare id as `{"id": …}`.
pub fn cli_params(args: &[String]) -> Result<Value> {
    match args {
        [] => Ok(Value::Null),
        [one] if one.trim_start().starts_with('{') => {
            serde_json::from_str(one).context("params must be a JSON object")
        }
        [id] => Ok(json!({ "id": id })),
        _ => bail!("give params as one JSON object argument"),
    }
}
