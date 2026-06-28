use crate::bridge::model::{BridgeRecord, deserialize_record, serialize_record};
use miette::{IntoDiagnostic, Result};
#[cfg(windows)]
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

enum IpcStream {
    #[cfg(windows)]
    NamedPipe(File),
    #[cfg(not(windows))]
    UnixSocket(std::os::unix::net::UnixStream),
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            IpcStream::NamedPipe(f) => f.read(buf),
            #[cfg(not(windows))]
            IpcStream::UnixSocket(s) => s.read(buf),
        }
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            IpcStream::NamedPipe(f) => f.write(buf),
            #[cfg(not(windows))]
            IpcStream::UnixSocket(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            IpcStream::NamedPipe(f) => f.flush(),
            #[cfg(not(windows))]
            IpcStream::UnixSocket(s) => s.flush(),
        }
    }
}

pub struct IpcClient {
    stream: IpcStream,
}

impl IpcClient {
    #[cfg(windows)]
    const PIPE_PATH: &'static str = r"\\.\pipe\aibrains-sync";

    #[cfg(not(windows))]
    const SOCKET_PATH: &'static str = "/tmp/aibrains-sync.sock";

    pub fn connect_with_timeout(timeout: Duration) -> Result<Self> {
        #[cfg(windows)]
        let path = Self::PIPE_PATH;
        #[cfg(not(windows))]
        let path = Self::SOCKET_PATH;
        Self::connect_to_path_with_timeout(path, timeout)
    }

    pub fn connect_to_path_with_timeout(path: &str, timeout: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let path_str = path.to_string();

        thread::spawn(move || {
            #[cfg(windows)]
            {
                let res = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path_str)
                    .map(IpcStream::NamedPipe);
                let _ = tx.send(res);
            }
            #[cfg(not(windows))]
            {
                let res =
                    std::os::unix::net::UnixStream::connect(&path_str).map(IpcStream::UnixSocket);
                let _ = tx.send(res);
            }
        });

        match rx.recv_timeout(timeout) {
            Ok(Ok(stream)) => Ok(Self { stream }),
            Ok(Err(e)) => Err(e).into_diagnostic(),
            Err(_) => Err(miette::miette!("Connection to AI-Brains IPC timed out.")),
        }
    }

    pub fn send_record(&mut self, record: &BridgeRecord) -> Result<()> {
        let line = serialize_record(record).into_diagnostic()?;
        self.stream.write_all(line.as_bytes()).into_diagnostic()?;
        self.stream.write_all(b"\n").into_diagnostic()?;
        self.stream.flush().into_diagnostic()?;
        Ok(())
    }

    pub fn receive_records(&mut self) -> Result<Vec<BridgeRecord>> {
        let mut reader = BufReader::new(&mut self.stream);
        let mut records = Vec::new();

        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).into_diagnostic()?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // End-of-response framing marker
                break;
            }
            let record = deserialize_record(trimmed).into_diagnostic()?;
            records.push(record);
        }

        Ok(records)
    }
}
