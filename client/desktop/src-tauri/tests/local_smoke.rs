use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SHELL_PORT: u16 = 19477;

struct AppGuard {
    child: Child,
    stderr: Option<std::process::ChildStderr>,
}

impl AppGuard {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AppGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn http_get(path: &str) -> std::io::Result<(u16, String)> {
    let address = ("127.0.0.1", SHELL_PORT)
        .to_socket_addrs()?
        .next()
        .expect("loopback address");
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{SHELL_PORT}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

fn wait_for_shell(timeout: Duration) -> (u16, String) {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        match http_get("/") {
            Ok((status, body)) if status == 200 => return (status, body),
            Ok((status, body)) => last = format!("status {status}: {body:.200}"),
            Err(error) => last = error.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("local shell did not start within {timeout:?}: {last}");
}

#[test]
#[ignore = "requires a display and the local Orqella daemon on 127.0.0.1:9477"]
fn fresh_profile_starts_shell_and_releases_gateway() {
    let root = std::env::temp_dir().join(format!("orqella-local-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for directory in ["config", "state", "data", "home"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_orqella-desktop"));
    command
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("GDK_BACKEND", "x11")
        .env("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1")
        .env("RUST_LOG", "orqella_desktop=info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("start Orqella desktop");
    let stderr = child.stderr.take();
    let mut app = AppGuard { child, stderr };

    let (status, body) = wait_for_shell(Duration::from_secs(45));
    assert_eq!(status, 200);
    assert!(
        body.to_lowercase().contains("orqella"),
        "unexpected shell: {body:.500}"
    );

    // Repeated shell reads prove the proxy stays up after the first request.
    for _ in 0..2 {
        let (status, _) = http_get("/").expect("shell request");
        assert_eq!(status, 200);
    }

    app.stop();

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], SHELL_PORT));
        if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_err() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let mut details = String::new();
    if let Some(stderr) = app.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut details);
    }
    panic!("gateway still listening on {SHELL_PORT} after quit: {details}");
}
