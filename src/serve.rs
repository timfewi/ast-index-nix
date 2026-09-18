//! Transports: stdio (direct), a Unix socket service and a stdin/stdout proxy.
//!
//! All three speak the same newline-delimited JSON-RPC framing. The socket is
//! the persistent-service shape (warm index, shared by several harnesses); the
//! proxy is the only harness-facing process and mirrors the design used by the
//! local research service. The service holds an exclusive `flock` so a second
//! writer cannot open the same index, and the socket lives at mode 0600.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nix::fcntl::{Flock, FlockArg};

use crate::error::{Error, Result};
use crate::rpc::RpcServer;

/// Options for the socket service.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub socket: PathBuf,
    pub lock: Option<PathBuf>,
}

/// Run the blocking stdio MCP loop until stdin closes.
pub fn run_stdio(server: &RpcServer) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| Error::io("<stdin>", error))?;
        if let Some(response) = server.handle_line(&line) {
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
    let reader = BufReader::new(stream.try_clone().map_err(|e| Error::io("<socket>", e))?);
    let mut writer = stream;
    for line in reader.lines() {
        let line = line.map_err(|error| Error::io("<socket>", error))?;
        let response = {
            let server = server
                .lock()
                .map_err(|_| Error::Invalid("service lock poisoned".to_string()))?;
            server.handle_line(&line)
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

    let reader_thread = std::thread::spawn(move || {
        let reader = BufReader::new(read_stream);
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if out.write_all(line.as_bytes()).is_err() || out.write_all(b"\n").is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    });

    let stdin = std::io::stdin();
    let mut writer = stream;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        writer
            .write_all(line.as_bytes())
            .map_err(|error| Error::io(socket, error))?;
        writer
            .write_all(b"\n")
            .map_err(|error| Error::io(socket, error))?;
        writer.flush().map_err(|error| Error::io(socket, error))?;
    }
    // stdin closed: stop reading from the service as well.
    let _ = writer.shutdown(std::net::Shutdown::Write);
    let _ = reader_thread.join();
    Ok(())
}
