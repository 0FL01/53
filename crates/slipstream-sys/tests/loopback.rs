//! Explicit synthetic C/Rust gate using the pinned Meson fixture executable.
use slipstream_sys::{Config, NativeClient, Status, StreamSnapshot};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(client: &NativeClient, predicate: impl Fn(Status) -> bool, seconds: u64) -> Status {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let status = client.status();
        if predicate(status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "native status deadline: {status:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn wait_frontier(client: &NativeClient, generation: u64, bytes: u64) -> StreamSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = client.stream_snapshot() {
            assert_eq!(value.generation, generation);
            if value.tx_submitted == bytes
                && value.rx_contiguous == bytes
                && value.rx_flushed == bytes
            {
                assert_eq!(value.rx_pending, 0);
                assert_eq!(value.rx_highest, bytes);
                assert!(!value.failed);
                assert_eq!(value.schema_version, 2);
                assert!(value.now_native_us > 0);
                return value;
            }
        }
        assert!(Instant::now() < deadline, "selected lane frontier deadline");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires DMSG_TEST_SERVER pointing to the pinned Meson slipstream-server"]
fn ready_full_cert_pin_streams_and_terminal_transport_loss() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/slipstream");
    let server_binary = std::env::var_os("DMSG_TEST_SERVER").expect("synthetic Meson server path");
    let certificate = std::fs::read(root.join("certs/cert.pem")).unwrap();
    let der = Command::new("openssl")
        .env_clear()
        .args(["x509", "-outform", "DER", "-in"])
        .arg(root.join("certs/cert.pem"))
        .output()
        .unwrap();
    assert!(der.status.success());
    assert!(der.stderr.is_empty());
    let backend = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = backend.local_addr().unwrap();
    let echo = thread::spawn(move || {
        // Two diagnostic lanes, followed by the original eight parallel streams.
        let streams: Vec<_> = (0..10)
            .map(|_| {
                let (mut socket, _) = backend.accept().unwrap();
                thread::spawn(move || {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut input = [0; 4096];
                    loop {
                        let n = socket.read(&mut input).unwrap();
                        if n == 0 {
                            break;
                        }
                        socket.write_all(&input[..n]).unwrap();
                    }
                })
            })
            .collect();
        for stream in streams {
            stream.join().unwrap();
        }
    });
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let resolver = udp.local_addr().unwrap();
    drop(udp);
    let mut server = Server(
        Command::new(server_binary)
            .env_clear()
            .arg("--dns-listen-host")
            .arg("127.0.0.1")
            .arg("--dns-listen-port")
            .arg(resolver.port().to_string())
            .arg("--target-address")
            .arg(target.to_string())
            .arg("--domain")
            .arg("native-fixture.invalid")
            .arg("--cert")
            .arg(root.join("certs/cert.pem"))
            .arg("--key")
            .arg(root.join("certs/key.pem"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    thread::sleep(Duration::from_millis(150));
    assert!(
        server.0.try_wait().unwrap().is_none(),
        "synthetic server exited"
    );

    // An otherwise valid full certificate from another public fixture must not
    // authenticate the carrier, even though QUIC/TLS itself can reach it.
    let wrong = std::fs::read(root.join("subprojects/picoquic/certs/test-ca.crt")).unwrap();
    assert!(wrong != certificate, "public wrong-pin fixture must differ");
    let mut client = NativeClient::start(Config::new(
        "native-fixture.invalid".into(),
        vec![resolver],
        wrong,
    ))
    .unwrap();
    assert_eq!(
        wait(&client, |s| matches!(s, Status::Failed(_)), 5),
        Status::Failed(7)
    );
    assert_eq!(client.stop().unwrap(), Status::Failed(7));

    // Both supported encodings install the exact upstream full-leaf verifier.
    let mut client = NativeClient::start(Config::new(
        "native-fixture.invalid".into(),
        vec![resolver],
        certificate,
    ))
    .unwrap();
    assert!(matches!(
        wait(&client, |s| matches!(s, Status::Ready(_)), 5),
        Status::Ready(_)
    ));
    assert_eq!(client.stop().unwrap(), Status::Stopped);
    let mut client = NativeClient::start(Config::new(
        "native-fixture.invalid".into(),
        vec![resolver],
        der.stdout,
    ))
    .unwrap();
    let Status::Ready(endpoint) = wait(&client, |s| matches!(s, Status::Ready(_)), 5) else {
        unreachable!()
    };
    // Two live lanes with distinct lengths establish local Rust TCP write ->
    // real worker QUIC offsets -> local socket flush -> Rust read. A diagnostic
    // selector never changes lane policy; this synthetic direct-authority path
    // is not device/recursive acceptance or an application-auth timing proof.
    let mut selected: Vec<_> = (0..2)
        .map(|_| {
            let socket = TcpStream::connect(endpoint).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
        })
        .collect();
    for (lane, size) in [(0, 317), (1, 911)] {
        let data = vec![lane as u8; size];
        selected[lane].write_all(&data).unwrap();
        let mut echo = vec![0; size];
        selected[lane].read_exact(&mut echo).unwrap();
        assert_eq!(echo, data);
    }
    for (lane, generation, bytes) in [(0, 1, 317), (1, 2, 911), (0, 3, 317)] {
        client.select_stream(Some((
            selected[lane].local_addr().unwrap().port(),
            generation,
        )));
        if let Some(value) = client.stream_snapshot() {
            assert_eq!(value.generation, generation); // Never a former selection copy.
        }
        wait_frontier(&client, generation, bytes);
    }
    client.select_stream(None);
    assert!(client.stream_snapshot().is_none());
    for mut socket in selected {
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        let mut tail = Vec::new();
        socket.read_to_end(&mut tail).unwrap();
        assert!(tail.is_empty());
    }
    let streams: Vec<_> = (0..8)
        .map(|i| {
            thread::spawn(move || {
                let mut local = TcpStream::connect(endpoint).unwrap();
                local
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                local
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let data = vec![i as u8; 8192];
                local.write_all(&data).unwrap();
                local.shutdown(std::net::Shutdown::Write).unwrap();
                let mut reply = Vec::new();
                local.read_to_end(&mut reply).unwrap();
                assert_eq!(reply, data);
            })
        })
        .collect();
    for stream in streams {
        stream.join().unwrap();
    }
    echo.join().unwrap();
    // Kill only the disposable fixture process. The native worker itself is
    // never signalled or force-killed, and must not reconnect after ready loss.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let started = Instant::now();
    let failed = wait(&client, |s| matches!(s, Status::Failed(_)), 40);
    assert_eq!(failed, Status::Failed(1));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(client.status(), failed);
    assert_eq!(client.stop().unwrap(), failed);
    eprintln!("native loopback: PEM/DER ready, wrong pin rejected, 8 exact raw streams, terminal loss in {:?}", started.elapsed());
}
