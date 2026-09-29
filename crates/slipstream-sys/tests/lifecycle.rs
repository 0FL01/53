use slipstream_sys::{Config, Error, NativeClient, Status};
use std::{
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

const PEM: &[u8] = include_bytes!("../../../vendor/slipstream/certs/cert.pem");

fn config(resolver: std::net::SocketAddr) -> Config {
    Config::new(
        "native-fixture.invalid".into(),
        vec![resolver],
        PEM.to_vec(),
    )
}
fn wait(client: &NativeClient, predicate: impl Fn(Status) -> bool) -> Status {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client.status();
        if predicate(status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "native status deadline: {status:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}
fn tasks() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}
fn handler(signal: i32) -> libc::sigaction {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        action
    }
}

#[test]
fn native_lifecycle_errors_cancel_races_and_resource_reclamation() {
    // A bound, unread UDP sink prevents accidental external traffic and ICMP
    // failures. It never responds, so Listening must never be called Ready.
    let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
    let profile = config(sink.local_addr().unwrap());
    let sigint = handler(libc::SIGINT);
    let sigterm = handler(libc::SIGTERM);

    let mut bad = profile.clone();
    bad.domain = "bad\0domain".into();
    assert!(matches!(
        NativeClient::start(bad),
        Err(Error::InvalidConfig("domain"))
    ));
    let mut bad = profile.clone();
    bad.resolvers = vec![];
    assert!(matches!(
        NativeClient::start(bad),
        Err(Error::InvalidConfig(_))
    ));
    let mut bad = profile.clone();
    bad.resolvers.push("[::1]:53".parse().unwrap());
    assert!(matches!(
        NativeClient::start(bad),
        Err(Error::InvalidConfig(_))
    ));

    let mut bad = profile.clone();
    bad.certificate = vec![0x30, 0];
    let mut client = NativeClient::start(bad).unwrap();
    assert_eq!(
        wait(&client, |s| matches!(s, Status::Failed(_))),
        Status::Failed(4)
    );
    assert!(client.endpoint().is_none());
    assert!(matches!(
        NativeClient::start(profile.clone()),
        Err(Error::AlreadyRunning)
    ));
    // A late stop never rewrites a completed setup failure as Stopped.
    assert_eq!(client.stop().unwrap(), Status::Failed(4));
    assert_eq!(client.stop().unwrap(), Status::Failed(4));

    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut bad = profile.clone();
    bad.listen_port = occupied.local_addr().unwrap().port();
    let mut client = NativeClient::start(bad).unwrap();
    assert_eq!(
        wait(&client, |s| matches!(s, Status::Failed(_))),
        Status::Failed(5)
    );
    assert_eq!(client.stop().unwrap(), Status::Failed(5));
    drop(occupied);

    let mut client = NativeClient::start(profile.clone()).unwrap();
    let listening = wait(&client, |s| matches!(s, Status::Listening(_)));
    let Status::Listening(endpoint) = listening else {
        unreachable!()
    };
    assert!(endpoint.ip().is_loopback());
    assert_ne!(endpoint.port(), 0);
    let local = TcpStream::connect(endpoint).unwrap();
    let started = Instant::now();
    // Cancellation after bind but before ready must not wait for the 3s
    // bootstrap timeout, require a signal, or mutate should_shutdown.
    assert_eq!(client.stop().unwrap(), Status::Stopped);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(local);
    drop(TcpListener::bind(endpoint).expect("native listener closed after join"));

    let mut client = NativeClient::start(profile.clone()).unwrap();
    assert_eq!(
        wait(&client, |s| matches!(s, Status::Failed(_))),
        Status::Failed(1)
    );
    assert_eq!(client.stop().unwrap(), Status::Failed(1));

    // Simultaneous starts retain their handles until all competitors finish.
    let barrier = Arc::new(Barrier::new(16));
    let (tx, rx) = mpsc::channel();
    let contenders: Vec<_> = (0..16)
        .map(|_| {
            let barrier = barrier.clone();
            let tx = tx.clone();
            let profile = profile.clone();
            thread::spawn(move || {
                barrier.wait();
                tx.send(NativeClient::start(profile)).unwrap();
            })
        })
        .collect();
    drop(tx);
    let mut winners = Vec::new();
    let mut rejected = 0;
    for result in rx {
        match result {
            Ok(client) => winners.push(client),
            Err(Error::AlreadyRunning) => rejected += 1,
            Err(e) => panic!("unexpected start error: {e}"),
        }
    }
    for contender in contenders {
        contender.join().unwrap();
    }
    assert_eq!(winners.len(), 1);
    assert_eq!(rejected, 15);
    assert_eq!(winners[0].stop().unwrap(), Status::Stopped);
    drop(winners);

    let baseline_fds = fd_count();
    let baseline_tasks = tasks();
    let mut max_stop = Duration::ZERO;
    for i in 0..64 {
        let mut profile = profile.clone();
        if i % 2 == 0 {
            profile.resolvers = vec![sink.local_addr().unwrap(); 8];
        }
        let mut client = NativeClient::start(profile).unwrap();
        if i % 3 == 0 {
            wait(&client, |s| matches!(s, Status::Listening(_)));
        }
        let start = Instant::now();
        thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..100 {
                        client.request_stop();
                        let _ = client.status();
                    }
                });
            }
        });
        assert_eq!(client.stop().unwrap(), Status::Stopped);
        max_stop = max_stop.max(start.elapsed());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(client.endpoint().is_none());
        assert_eq!(fd_count(), baseline_fds, "FD leak in cycle {i}");
    }
    // Linux task disappearance may lag pthread_join very briefly.
    let deadline = Instant::now() + Duration::from_secs(1);
    while tasks() != baseline_tasks && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(tasks(), baseline_tasks);
    assert_eq!(handler(libc::SIGINT).sa_sigaction, sigint.sa_sigaction);
    assert_eq!(handler(libc::SIGINT).sa_flags, sigint.sa_flags);
    assert_eq!(handler(libc::SIGTERM).sa_sigaction, sigterm.sa_sigaction);
    assert_eq!(handler(libc::SIGTERM).sa_flags, sigterm.sa_flags);

    // Drop has the same stop/join guarantee and allows the next run.
    drop(NativeClient::start(profile.clone()).unwrap());
    let mut client = NativeClient::start(profile).unwrap();
    assert_eq!(client.stop().unwrap(), Status::Stopped);
    eprintln!("native lifecycle: 64 cycles, max cancel/join {max_stop:?}, FD/task counts stable");
}
