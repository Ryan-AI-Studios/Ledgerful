use ledgerful::bridge::ipc::IpcClient;
#[cfg(windows)]
use std::fs;
use std::time::Duration;

#[test]
fn test_ipc_client_timeout_on_nonexistent_pipe() {
    // This should not hang
    #[cfg(windows)]
    let path = r"\\.\pipe\this-pipe-does-not-exist-12345678";
    #[cfg(not(windows))]
    let path = "/tmp/this-socket-does-not-exist-12345678";

    let client = IpcClient::connect_to_path_with_timeout(path, Duration::from_millis(100));
    assert!(client.is_err());
}

#[cfg(windows)]
#[test]
#[ignore = "requires deterministic IPC readiness probe — see TA8"]
fn test_ipc_receive_records_staggered() {
    use crate::common::wait_for_condition;
    use ledgerful::bridge::model::{
        BridgeDirection, BridgePayload, BridgeRecord, serialize_record,
    };
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::RawHandle;
    use std::os::windows::prelude::*;
    use std::thread;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    // Convert pipe name to UTF-16
    let pipe_name: Vec<u16> = r"\\.\pipe\aibrains-sync"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Create the named pipe server
    let server_handle = unsafe {
        CreateNamedPipeW(
            pipe_name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            1024,
            1024,
            0,
            std::ptr::null(),
        )
    };

    assert_ne!(server_handle, INVALID_HANDLE_VALUE);

    // Cast raw handle pointer to isize (which is Send) to pass across thread boundary safely
    let server_handle_raw = server_handle as isize;

    // Spawn server thread
    let server_thread = thread::spawn(move || {
        let server_handle = server_handle_raw as windows_sys::Win32::Foundation::HANDLE;
        let connected = unsafe { ConnectNamedPipe(server_handle, std::ptr::null_mut()) };
        if connected == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(997) {
                // ERROR_IO_PENDING is fine
                panic!("ConnectNamedPipe failed: {}", err);
            }
        }

        // Convert the handle to a File
        let mut file = unsafe { File::from_raw_handle(server_handle as RawHandle) };

        let records: Vec<(&str, BridgePayload)> = vec![
            (
                "insight",
                BridgePayload::Insight {
                    memory_id: "m1".to_string(),
                    relevance: 1.0,
                    content: "insight 1".to_string(),
                },
            ),
            (
                "insight",
                BridgePayload::Insight {
                    memory_id: "m2".to_string(),
                    relevance: 0.8,
                    content: "insight 2".to_string(),
                },
            ),
            (
                "hotspot_delta",
                BridgePayload::Hotspot {
                    path: "src/main.rs".to_string(),
                    score: 0.5,
                    reason: "high score".to_string(),
                    temporal_coupling: 0.0,
                    failure_risk_probability: 0.0,
                },
            ),
        ];

        for (kind, payload) in records {
            let record = BridgeRecord::new(
                BridgeDirection::Inbound,
                "test-project".to_string(),
                kind,
                payload,
            );
            let line = serialize_record(&record).unwrap() + "\n";
            file.write_all(line.as_bytes()).unwrap();
            file.flush().unwrap();

            // Stagger delivery with a tiny pause so the client exercises the
            // stream-read timeout path rather than receiving everything in one
            // big buffer. This pause is bounded and small; the client loop will
            // block on its own timeout between records.
            thread::sleep(Duration::from_millis(5));
        }

        // Drop the file to close the pipe handle, sending EOF to client
        drop(file);

        // Cleanup
        unsafe {
            DisconnectNamedPipe(server_handle);
        }
    });

    // Wait a brief moment for server to start ConnectNamedPipe
    wait_for_condition(
        || fs::metadata(r"\\.\pipe\aibrains-sync").is_ok(),
        Duration::from_secs(2),
        Duration::from_millis(50),
    )
    .expect("named pipe server did not become ready within 2s");

    // Connect client
    let mut client = IpcClient::connect_with_timeout(Duration::from_secs(2)).unwrap();

    // Read records
    let records = client.receive_records().unwrap();

    server_thread.join().unwrap();

    assert_eq!(records.len(), 3);
    assert_eq!(records[0].record_kind, "insight");
    assert_eq!(records[1].record_kind, "insight");
    assert_eq!(records[2].record_kind, "hotspot_delta");
}
