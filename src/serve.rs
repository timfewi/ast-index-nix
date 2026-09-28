//! Transports: stdio (direct), a Unix socket service and a stdin/stdout proxy.
//!
//! All three speak the same newline-delimited JSON-RPC framing. The socket is
//! the persistent-service shape (warm index, shared by several harnesses); the
//! proxy is the only harness-facing process and mirrors the design used by the
//! local research service. The service holds an exclusive `flock` so a second
//! writer cannot open the same index, and the socket lives at mode 0600.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nix::fcntl::{Flock, FlockArg};

use crate::error::{Error, Result};
use crate::rpc::RpcServer;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const OVERSIZED_FRAME_RESPONSE: &str = "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600,\"message\":\"request frame exceeds 1048576 bytes\"}}";

enum Frame {
    Line(String),
    TooLarge,
}

/// Drain the whole line, but retain at most one bounded JSON-RPC frame.
fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut bytes = Vec::new();
    let mut too_large = false;
    let mut saw_data = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !saw_data {
                return Ok(None);
            }
            break;
        }
        saw_data = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let body = &available[..newline.unwrap_or(consumed)];
        if !too_large {
            if body.len() > MAX_FRAME_BYTES.saturating_sub(bytes.len()) {
                too_large = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(body);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if too_large {
        return Ok(Some(Frame::TooLarge));
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let line = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(Frame::Line(line)))
}

fn frame_response(server: &RpcServer, frame: Frame) -> Option<String> {
    match frame {
        Frame::Line(line) => server.handle_line(&line),
        Frame::TooLarge => Some(OVERSIZED_FRAME_RESPONSE.to_string()),
    }
}

fn copy_flushed(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..count])?;
        writer.flush()?;
    }
}

/// Options for the socket service.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub socket: PathBuf,
    pub lock: Option<PathBuf>,
}

/// Run the blocking stdio MCP loop until stdin closes.
pub fn run_stdio(server: &RpcServer) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = std::io::stdout();
    while let Some(frame) = read_frame(&mut reader).map_err(|error| Error::io("<stdin>", error))? {
        if let Some(response) = frame_response(server, frame) {
            writeln!(stdout, "{response}").map_err(|error| Error::io("<stdout>", error))?;
            stdout
                .flush()
                .map_err(|error| Error::io("<stdout>", error))?;
        }
    }
    Ok(())
}

/// Run the Unix socket service. Blocks until the process is stopped.
pub fn run_socket(server: RpcServer, options: &ServeOptions) -> Result<()> {
    let lock_path = options
        .lock
        .clone()
        .unwrap_or_else(|| options.socket.with_extension("lock"));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| Error::io(&lock_path, error))?;
    let _guard = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, error)| Error::io(&lock_path, error.into()))?;

    if options.socket.exists() {
        // A socket file without a lock holder is stale.
        std::fs::remove_file(&options.socket).map_err(|error| Error::io(&options.socket, error))?;
    }
    let listener =
        UnixListener::bind(&options.socket).map_err(|error| Error::io(&options.socket, error))?;
    std::fs::set_permissions(&options.socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| Error::io(&options.socket, error))?;

    eprintln!("ast-index: serving on {}", options.socket.display());
    let server = Arc::new(Mutex::new(server));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    if let Err(error) = serve_connection(stream, &server) {
                        eprintln!("ast-index: connection ended: {error}");
                    }
                });
            }
            Err(error) => eprintln!("ast-index: accept failed: {error}"),
        }
    }
    Ok(())
}

fn serve_connection(stream: UnixStream, server: &Arc<Mutex<RpcServer>>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| Error::io("<socket>", e))?);
    let mut writer = stream;
    while let Some(frame) = read_frame(&mut reader).map_err(|error| Error::io("<socket>", error))? {
        let response = {
            let server = server
                .lock()
                .map_err(|_| Error::Invalid("service lock poisoned".to_string()))?;
            frame_response(&server, frame)
        };
        if let Some(response) = response {
            writer
                .write_all(response.as_bytes())
                .map_err(|error| Error::io("<socket>", error))?;
            writer
                .write_all(b"\n")
                .map_err(|error| Error::io("<socket>", error))?;
            writer
                .flush()
                .map_err(|error| Error::io("<socket>", error))?;
        }
    }
    Ok(())
}

/// Proxy stdin/stdout to a running socket service, byte for byte.
pub fn run_proxy(socket: &Path) -> Result<()> {
    let stream = UnixStream::connect(socket).map_err(|error| Error::io(socket, error))?;
    let read_stream = stream
        .try_clone()
        .map_err(|error| Error::io(socket, error))?;

    let reader_thread = std::thread::spawn(move || -> std::io::Result<()> {
        let mut reader = read_stream;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        copy_flushed(&mut reader, &mut out)
    });

    let stdin = std::io::stdin();
    let mut writer = stream;
    let transfer = copy_flushed(&mut stdin.lock(), &mut writer);
    // stdin closed: stop reading from the service as well.
    let _ = writer.shutdown(std::net::Shutdown::Write);
    let output = reader_thread
        .join()
        .map_err(|_| Error::Invalid("proxy output thread panicked".to_string()))?;
    transfer.map_err(|error| Error::io(socket, error))?;
    output.map_err(|error| Error::io("<stdout>", error))?;
    Ok(())
}
