//! `zed-editor-mcp` library: a stdio↔Unix-socket relay for the Zed editor's MCP
//! server. MCP clients spawn stdio servers; the editor listens on a socket —
//! this binary is the shim between the two, and deliberately nothing more:
//! frame parsing and dispatch live in the editor (`crates/editor_mcp`).

use std::io::{BufRead, Write};
use std::io::BufRead as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const DEFAULT_SOCKET_ENV: &str = "ZED_EDITOR_MCP_SOCKET";



pub fn parse_args<I>(args: I) -> Result<PathBuf, String>
where
    I: Iterator<Item = String>,
{
    let mut args = args.skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => return Ok(PathBuf::from(path)),
                None => return Err("--socket requires a path".to_string()),
            },
            "--help" | "-h" => {
                return Err(usage_string());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    std::env::var_os(DEFAULT_SOCKET_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("no --socket given and {DEFAULT_SOCKET_ENV} is not set")
        })
}

pub fn usage_string() -> String {
    format!("usage: zed-editor-mcp [--socket <path>] (default: ${DEFAULT_SOCKET_ENV})",)
}

pub fn connect_and_run(socket_path: &std::path::Path) -> Result<(), String> {
    let to_server =
        UnixStream::connect(socket_path).map_err(|error| format!("connect failed: {error}"))?;
    let read_half = to_server
        .try_clone()
        .map_err(|error| format!("socket clone failed: {error}"))?;
    // Stdin/Stdout are their own BufRead/Write handles and are thread-safe;
    // locks would not cross the relay's stdin thread.
    relay(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        to_server,
        read_half,
    )
}

/// Relays stdin lines to the socket and socket lines to `stdout` until either
/// side reaches EOF. Never parses JSON: framing only.
pub fn relay(
    stdin: impl BufRead + Send + 'static,
    mut stdout: impl Write,
    mut to_server: UnixStream,
    read_half: UnixStream,
) -> Result<(), String> {
    let stdin_relay = std::thread::Builder::new()
        .name("mcp-stdin-relay".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(stdin);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                if to_server.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
            let _ = to_server.shutdown(std::net::Shutdown::Write);
        })
        .map_err(|error| format!("could not spawn stdin relay: {error}"))?;

    let socket_reader = std::io::BufReader::new(read_half);
    for line in socket_reader.lines() {
        let line = line.map_err(|error| format!("socket read failed: {error}"))?;
        if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
    let _ = stdin_relay.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::sync::mpsc;

    #[test]
    fn parses_socket_flag_and_help() {
        let args = vec![
            "zed-editor-mcp".to_string(),
            "--socket".to_string(),
            "/tmp/x.sock".to_string(),
        ];
        assert_eq!(
            parse_args(args.into_iter()),
            Ok(PathBuf::from("/tmp/x.sock"))
        );
        let args = vec!["zed-editor-mcp".to_string(), "--help".to_string()];
        assert_eq!(
            parse_args(args.into_iter()),
            Err("usage: zed-editor-mcp [--socket <path>] (default: $ZED_EDITOR_MCP_SOCKET)"
                .to_string())
        );
        let args = vec!["zed-editor-mcp".to_string(), "-q".to_string()];
        assert_eq!(
            parse_args(args.into_iter()),
            Err("unknown argument: -q".to_string())
        );
        let args = vec!["zed-editor-mcp".to_string()];
        assert_eq!(
            parse_args(args.into_iter()).err().unwrap(),
            format!("no --socket given and {DEFAULT_SOCKET_ENV} is not set")
        );
    }

    #[test]
    fn relays_both_directions_until_eof() {
        // Wire the proxy's "socket" side to a fake editor in-thread.
        let script_stdin = "{\"id\":1}\n{\"id\":2}\n";
        let (done_tx, done_rx) = mpsc::channel::<Vec<String>>();
        let (to_proxy, editor_side) = UnixStream::pair().unwrap();

        // Fake editor: reads frames until EOF, echoes each, then closes.
        std::thread::spawn(move || {
            let mut echoed = Vec::new();
            for line in BufReader::new(editor_side.try_clone().unwrap())
                .lines()
                .map_while(Result::ok)
            {
                echoed.push(line);
            }
            let mut side = editor_side;
            for line in echoed {
                writeln!(side, "ack:{line}").unwrap();
            }
            done_tx
                .send(Vec::<String>::new())
                .map_err(|_| ())
                .unwrap();
        });

        let proxy_stream = to_proxy.try_clone().unwrap();
        let stdout = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let stdout_writer = stdout.clone();
        let relay_handle = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(script_stdin.as_bytes());
            let mut writer = stdout_writer.lock().unwrap();
            relay(reader, &mut *writer, to_proxy, proxy_stream).unwrap();
        });
        relay_handle.join().unwrap();

        // The fake editor saw both frames in order, and every frame produced
        // a framing-preserved reply on stdout.
        let mut output = String::new();
        {
            let bytes = stdout.lock().unwrap().clone();
            output.push_str(&String::from_utf8(bytes).unwrap());
        }
        assert_eq!(
            output,
            "ack:{\"id\":1}\nack:{\"id\":2}\n",
            "proxied stdout must frame-perfectly mirror the socket"
        );
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
    }
}
