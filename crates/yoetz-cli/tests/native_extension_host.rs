#![cfg(unix)]

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_BYTES: usize = 192 * 1024;

#[test]
fn native_host_multiplexes_clients_and_isolates_disconnect_cancellation() {
    let mut host = NativeHost::start();
    let token = wait_for_token(&host.token_path);

    let bundle_a = write_bundle(host.temp.path(), "a.md", CHUNK_BYTES + 11, b'a');
    let bundle_b = write_bundle(host.temp.path(), "b.md", CHUNK_BYTES + 17, b'b');
    let client_a = LocalClient::connect(&host.socket_path, "job_a", &bundle_a, &token);
    let client_b = LocalClient::connect(&host.socket_path, "job_b", &bundle_b, &token);

    host.output.take("job_start", "job_a");
    host.output.take("job_start", "job_b");

    host.send(extension_frame(
        "job_progress",
        "job_a",
        json!({"phase": "ready_for_file"}),
    ));
    client_a.take("job_progress");
    assert_chunk(&host.output.take("job_file_chunk", "job_a"), 0, 2);

    host.send(extension_frame(
        "job_progress",
        "job_b",
        json!({"phase": "ready_for_file"}),
    ));
    client_b.take("job_progress");
    assert_chunk(&host.output.take("job_file_chunk", "job_b"), 0, 2);

    host.send(extension_frame(
        "job_file_chunk_ack",
        "job_a",
        json!({"sequence": 0, "complete": false}),
    ));
    client_a.take("job_file_chunk_ack");
    assert_chunk(&host.output.take("job_file_chunk", "job_a"), 1, 2);

    host.send(extension_frame(
        "job_file_chunk_ack",
        "job_b",
        json!({"sequence": 0, "complete": false}),
    ));
    client_b.take("job_file_chunk_ack");
    assert_chunk(&host.output.take("job_file_chunk", "job_b"), 1, 2);

    host.send(extension_frame(
        "job_file_chunk_ack",
        "job_b",
        json!({"sequence": 1, "complete": true}),
    ));
    client_b.take("job_file_chunk_ack");
    host.send(extension_frame(
        "job_complete",
        "job_b",
        json!({"response": "B finished first"}),
    ));
    assert_eq!(
        client_b.take("job_complete")["payload"]["response"],
        "B finished first"
    );

    host.send(extension_frame(
        "job_file_chunk_ack",
        "job_a",
        json!({"sequence": 1, "complete": true}),
    ));
    client_a.take("job_file_chunk_ack");
    host.send(extension_frame(
        "job_complete",
        "job_a",
        json!({"response": "A finished second"}),
    ));
    assert_eq!(
        client_a.take("job_complete")["payload"]["response"],
        "A finished second"
    );

    let bundle_c = write_bundle(host.temp.path(), "c.md", 4, b'c');
    let bundle_d = write_bundle(host.temp.path(), "d.md", 4, b'd');
    let client_c = LocalClient::connect(&host.socket_path, "job_c", &bundle_c, &token);
    let client_d = LocalClient::connect(&host.socket_path, "job_d", &bundle_d, &token);
    host.output.take("job_start", "job_c");
    host.output.take("job_start", "job_d");

    host.send(extension_frame(
        "job_progress",
        "job_c",
        json!({"phase": "ready_for_file"}),
    ));
    assert_eq!(client_c.take("job_progress")["job_id"], "job_c");
    assert_chunk(&host.output.take("job_file_chunk", "job_c"), 0, 1);
    host.send(extension_frame(
        "job_progress",
        "job_d",
        json!({"phase": "ready_for_file"}),
    ));
    client_d.take("job_progress");
    assert_chunk(&host.output.take("job_file_chunk", "job_d"), 0, 1);

    drop(client_c);
    host.send(extension_frame(
        "job_file_chunk_ack",
        "job_d",
        json!({"sequence": 0, "complete": true}),
    ));
    client_d.take("job_file_chunk_ack");
    host.send(extension_frame(
        "job_complete",
        "job_d",
        json!({"response": "D survived C disconnect"}),
    ));
    assert_eq!(
        client_d.take("job_complete")["payload"]["response"],
        "D survived C disconnect"
    );

    let cancel = host.output.take("job_cancel", "job_c");
    assert_eq!(cancel["payload"]["reason"], "local_client_disconnected");
}

struct NativeHost {
    child: Child,
    stdin: ChildStdin,
    output: FrameInbox,
    temp: TempDir,
    socket_path: PathBuf,
    token_path: PathBuf,
}

impl NativeHost {
    fn start() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let socket_path = temp.path().join("native.sock");
        let token_path = state_dir
            .join("chrome-extension-native")
            .join("chatgpt-native.token");
        let mut child = Command::new(env!("CARGO_BIN_EXE_yoetz"))
            .args(["browser", "chrome-native-host", "--chatgpt"])
            .env("YOETZ_DIR", &state_dir)
            .env("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET", &socket_path)
            .env(
                "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
                temp.path().join("native-hosts"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let output = FrameInbox::from_reader(stdout);
        wait_for_path(&socket_path);
        Self {
            child,
            stdin,
            output,
            temp,
            socket_path,
            token_path,
        }
    }

    fn send(&mut self, frame: Value) {
        write_frame(&mut self.stdin, &frame);
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct LocalClient {
    stream: UnixStream,
    job_id: String,
}

impl LocalClient {
    fn connect(socket_path: &Path, job_id: &str, bundle_path: &Path, token: &str) -> Self {
        let mut stream = UnixStream::connect(socket_path).unwrap();
        stream.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
        let bundle_size = fs::metadata(bundle_path).unwrap().len();
        write_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "transport": "chrome-extension-native",
                "request_id": format!("req_start_{job_id}"),
                "job_id": job_id,
                "run_id": format!("run_{job_id}"),
                "workspace_id": "workspace_test",
                "capability_token": token,
                "type": "job_start",
                "payload": {
                    "recipe": "chatgpt",
                    "bundle_path": bundle_path,
                    "bundle_size": bundle_size,
                    "prompt": format!("prompt {job_id}"),
                    "wait_timeout_ms": 5000
                }
            }),
        );
        Self {
            stream,
            job_id: job_id.to_string(),
        }
    }

    fn take(&self, kind: &str) -> Value {
        let frame = read_frame(&mut &self.stream);
        assert_eq!(
            frame["job_id"], self.job_id,
            "client received another job's frame"
        );
        assert_eq!(frame["type"], kind, "client received an unexpected frame");
        frame
    }
}

struct FrameInbox {
    receiver: Receiver<Value>,
    pending: VecDeque<Value>,
}

impl FrameInbox {
    fn from_reader(mut reader: impl Read + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(frame) = try_read_frame(&mut reader) {
                if sender.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            receiver,
            pending: VecDeque::new(),
        }
    }

    fn take(&mut self, kind: &str, job_id: &str) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|frame| frame["type"] == kind && frame["job_id"] == job_id)
        {
            return self.pending.remove(index).unwrap();
        }
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = self
                .receiver
                .recv_timeout(remaining)
                .unwrap_or_else(|err| panic!("timed out waiting for {kind} for {job_id}: {err}"));
            if frame["type"] == kind && frame["job_id"] == job_id {
                return frame;
            }
            self.pending.push_back(frame);
        }
    }
}

fn extension_frame(kind: &str, job_id: &str, payload: Value) -> Value {
    json!({
        "protocol_version": 1,
        "transport": "chrome-extension-native",
        "request_id": format!("req_{kind}_{job_id}"),
        "job_id": job_id,
        "run_id": format!("run_{job_id}"),
        "workspace_id": "workspace_test",
        "type": kind,
        "payload": payload
    })
}

fn assert_chunk(frame: &Value, sequence: u64, total_chunks: u64) {
    assert_eq!(frame["payload"]["sequence"], sequence);
    assert_eq!(frame["payload"]["total_chunks"], total_chunks);
}

fn write_bundle(dir: &Path, name: &str, size: usize, byte: u8) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![byte; size]).unwrap();
    path
}

fn wait_for_token(path: &Path) -> String {
    wait_for_path(path);
    fs::read_to_string(path).unwrap().trim().to_string()
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn write_frame(writer: &mut impl Write, frame: &Value) {
    let bytes = serde_json::to_vec(frame).unwrap();
    writer
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .unwrap();
    writer.write_all(&bytes).unwrap();
    writer.flush().unwrap();
}

fn read_frame(reader: &mut impl Read) -> Value {
    try_read_frame(reader).unwrap()
}

fn try_read_frame(reader: &mut impl Read) -> std::io::Result<Value> {
    let mut len = [0_u8; 4];
    reader.read_exact(&mut len)?;
    let mut bytes = vec![0_u8; u32::from_ne_bytes(len) as usize];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}
