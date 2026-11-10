use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use kirby_csms_core::{CsmsConfig, CsmsServer};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

use super::limits::{DRAIN_BYTES, DRAIN_TIME, Limits, PAYLOAD_BUDGET};
use super::meta::ROUTES;
use super::server::StallIo;
use super::{bind, spawn, spawn_with};

/// The paths of `router()` are exactly the paths of `ROUTES`: the index lists every route
/// and no phantom one.
#[test]
fn routes_table_matches_the_router_definition() {
    let source = include_str!("mod.rs");
    let start = source.find("fn router(").unwrap();
    let end = start + source[start..].find("\n}\n").unwrap();
    let mut paths: Vec<&str> = Vec::new();
    let mut rest = &source[start..end];
    while let Some(at) = rest.find("\"/v1") {
        let tail = &rest[at + 1..];
        let close = tail.find('"').unwrap();
        paths.push(&tail[..close]);
        rest = &tail[close..];
    }
    let mut table: Vec<&str> = ROUTES.iter().map(|(path, _)| *path).collect();
    paths.sort_unstable();
    table.sort_unstable();
    assert_eq!(paths, table);
}

/// No limit gets in the way.
const LONG: Limits = Limits {
    write_stall: Duration::from_secs(600),
    response_deadline: Duration::from_secs(600),
    stream_max_age: Duration::from_secs(600),
    payload_budget: PAYLOAD_BUDGET,
};

#[tokio::test]
async fn bulk_admission_is_shared_and_rejects_before_any_call() {
    use kirby_ocpp_core::station::Snapshot;
    use kirby_ocpp_core::{Station, StationConfig, SystemClock};

    async fn post(addr: std::net::SocketAddr) -> String {
        let body = r#"{"station":"CP-*","action":"GetConfiguration"}"#;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!(
                    "POST /v1/calls HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        response
    }

    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let mut stations = Vec::new();
    for id in ["CP-a", "CP-b"] {
        let mut config = StationConfig::new(csms.url()).with_charge_point_id(id);
        config.persist = false;
        let (station, _) = Station::spawn(config, Arc::new(SystemClock));
        station.connect().await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(10),
            station.watch_snapshot().wait_for(Snapshot::booted),
        )
        .await
        .unwrap()
        .unwrap();
        stations.push(station);
    }
    let commands = crate::commands::CommandLimits {
        slots: Arc::new(Semaphore::new(2)),
        payload: Arc::new(Semaphore::new(1024 * 1024)),
    };
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api = spawn(listener, csms.clone(), commands.clone(), None, None);
    let held = commands.try_reserve(1, 0).unwrap();
    assert!(post(addr).await.contains(" 429 "));
    drop(held);
    let held = commands.try_reserve(0, 1024 * 1024).unwrap();
    assert!(post(addr).await.contains(" 429 "));
    for id in ["CP-a", "CP-b"] {
        assert!(!csms.history(id).unwrap().iter().any(|entry| matches!(&entry.frame, kirby_csms_core::Frame::Call { action, .. } if action == "GetConfiguration")));
    }
    drop(held);
    assert!(post(addr).await.contains(" 200 "));
    assert_eq!(commands.slots.available_permits(), 2);
    assert_eq!(commands.payload.available_permits(), 1024 * 1024);
    for station in stations {
        station.shutdown().await.unwrap();
    }
    api.finish().await;
    csms.shutdown().await;
}

/// A peer that takes one byte at a time, fast enough never to stall a write, cannot hold
/// a response longer than the response deadline; a streaming connection is exempt.
async fn slow_drain(streaming: bool) -> io::Result<()> {
    let (near, mut far) = tokio::io::duplex(1);
    let flag = Arc::new(AtomicBool::new(streaming));
    let mut io = StallIo::new(
        near,
        Limits {
            response_deadline: Duration::from_millis(300),
            ..LONG
        },
        flag,
    );
    let reader = tokio::spawn(async move {
        let mut byte = [0_u8; 1];
        while far.read(&mut byte).await.is_ok_and(|n| n > 0) {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    });
    let result = io.write_all(&[0_u8; 24]).await;
    drop(io);
    reader.await.unwrap();
    result
}

#[tokio::test]
async fn a_response_has_a_total_write_deadline() {
    let error = slow_drain(false).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn a_streaming_connection_has_no_total_write_deadline() {
    slow_drain(true).await.unwrap();
}

#[tokio::test]
async fn shutdown_drains_unread_request_data_but_only_so_much() {
    let (near, mut far) = tokio::io::duplex(8 * 1024);
    let mut io = StallIo::new(near, LONG, Arc::default());
    // A client that keeps sending: the server side must not wait for it to finish.
    let sender = tokio::spawn(async move {
        let chunk = [1_u8; 1024];
        let mut sent = 0;
        while far.write_all(&chunk).await.is_ok() {
            sent += chunk.len();
            if sent > 4 * 1024 * 1024 {
                break;
            }
        }
        sent
    });
    let started = Instant::now();
    io.shutdown().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(io.drained >= DRAIN_BYTES, "drained {}", io.drained);
    drop(io);
    assert!(sender.await.unwrap() < 4 * 1024 * 1024);
}

#[tokio::test]
async fn shutdown_returns_at_once_when_the_client_is_done() {
    let (near, far) = tokio::io::duplex(1024);
    drop(far);
    let mut io = StallIo::new(near, LONG, Arc::default());
    let started = Instant::now();
    io.shutdown().await.unwrap();
    assert!(started.elapsed() < DRAIN_TIME);
}

/// The bodies of calls in flight count against the budget; beyond it a request gets 429,
/// and a finished call gives its share back.
#[tokio::test]
async fn request_data_held_by_calls_in_flight_is_capped() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let station_addr = csms.local_addr();
    let mut silent = TcpStream::connect(station_addr).await.unwrap();
    silent
        .write_all(
            format!(
                "GET /ocpp/CP-1 HTTP/1.1\r\nHost: {station_addr}\r\nConnection: Upgrade\r\n\
                 Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                 Sec-WebSocket-Protocol: ocpp1.6\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut handshake = Vec::new();
    let mut byte = [0_u8; 1];
    while !handshake.ends_with(b"\r\n\r\n") {
        silent.read_exact(&mut byte).await.unwrap();
        handshake.push(byte[0]);
    }
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api = spawn_with(
        listener,
        csms.clone(),
        Arc::new(Semaphore::new(16)),
        None,
        Limits {
            payload_budget: 3000,
            ..LONG
        },
    );
    let post = move |timeout_ms: u32| async move {
        let body = json!({
            "action": "DataTransfer",
            "payload": {"vendorId": "x", "data": "a".repeat(1000)},
            "timeout_ms": timeout_ms,
        })
        .to_string();
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST /v1/stations/CP-1/calls HTTP/1.1\r\nHost: {addr}\r\n\
             Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        String::from_utf8_lossy(&raw).into_owned()
    };
    // Two calls fit (about 1.1 KB each); they wait for the silent station.
    // A held call that loses the race for the budget to a probe tries again.
    let waiting: Vec<_> = (0..2)
        .map(|_| {
            tokio::spawn(async move {
                while post(600_000).await.contains(" 429 ") {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        })
        .collect();
    // Probes that slip in before both are held give their share back after 300 ms.
    let started = Instant::now();
    loop {
        let answer = post(300).await;
        if answer.contains(" 429 ") {
            assert!(answer.contains("request data"), "{answer}");
            break;
        }
        assert!(answer.contains(" 200 "), "{answer}");
        assert!(started.elapsed() < Duration::from_secs(30), "never saw 429");
        // Without a pause the next probe grabs the share the last one gave back before a
        // held call (retrying every 20 ms) can: the loop would then take many seconds.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for task in waiting {
        task.abort();
    }
    api.finish().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn event_streams_end_at_their_maximum_age_with_a_note() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api = spawn_with(
        listener,
        csms.clone(),
        Arc::new(Semaphore::new(4)),
        None,
        Limits {
            stream_max_age: Duration::from_millis(300),
            ..LONG
        },
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET /v1/events HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("stream did not end")
        .ok();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("200 OK"), "{text}");
    assert!(text.contains("maximum age"), "{text}");
    api.finish().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_write_without_progress_times_out() {
    // The peer end of the pipe is never read: 8 bytes fit, the rest of the write blocks.
    let (near, _far) = tokio::io::duplex(8);
    let mut io = StallIo::new(
        near,
        Limits {
            write_stall: Duration::from_millis(150),
            ..LONG
        },
        Arc::default(),
    );
    let started = Instant::now();
    let error = io.write_all(&[0_u8; 64]).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(100));
}

#[tokio::test]
async fn a_slow_but_moving_write_is_left_alone() {
    let (near, mut far) = tokio::io::duplex(8);
    let mut io = StallIo::new(
        near,
        Limits {
            write_stall: Duration::from_millis(300),
            ..LONG
        },
        Arc::default(),
    );
    let reader = tokio::spawn(async move {
        let mut got = 0;
        let mut chunk = [0_u8; 8];
        while got < 64 {
            tokio::time::sleep(Duration::from_millis(40)).await;
            got += far.read(&mut chunk).await.unwrap();
        }
    });
    io.write_all(&[0_u8; 64]).await.unwrap();
    reader.await.unwrap();
}

#[tokio::test]
async fn finish_closes_connections_that_are_still_open() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api = spawn(
        listener,
        csms.clone(),
        crate::commands::CommandLimits {
            slots: Arc::new(Semaphore::new(4)),
            payload: Arc::new(Semaphore::new(PAYLOAD_BUDGET)),
        },
        None,
        None,
    );
    // One connection idle, one with a request body that never completes.
    let mut idle = TcpStream::connect(addr).await.unwrap();
    let mut stalled = TcpStream::connect(addr).await.unwrap();
    stalled
        .write_all(
            format!(
                "POST /v1/rules/Heartbeat HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 50\r\n\r\n{{"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let started = Instant::now();
    api.finish().await;
    assert!(started.elapsed() < Duration::from_secs(6));
    // The listener is gone with the server.
    assert!(TcpStream::connect(addr).await.is_err());
    for stream in [&mut idle, &mut stalled] {
        // Closed by the server: end of stream or a reset, not a hang.
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut rest))
            .await
            .expect("connection still open after finish");
        drop(read);
    }
    csms.shutdown().await;
}

/// Reads an event stream to its end; returns the text.
async fn read_stream(addr: std::net::SocketAddr, headers: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET /v1/events HTTP/1.1\r\nHost: {addr}\r\n{headers}\r\n").as_bytes())
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("stream did not end")
        .ok();
    String::from_utf8_lossy(&raw).into_owned()
}

/// The note that ends a stream at its maximum age carries an id a client can resume from:
/// reconnecting with it is no `resync`, and nothing between is lost.
#[tokio::test]
async fn the_id_of_the_maximum_age_note_resumes_the_stream() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api = spawn_with(
        listener,
        csms.clone(),
        Arc::new(Semaphore::new(4)),
        None,
        Limits {
            stream_max_age: Duration::from_millis(300),
            ..LONG
        },
    );
    let first = read_stream(addr, "").await;
    assert!(first.contains("maximum age"), "{first}");
    let last_id = first
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .next_back()
        .expect("the note has an id")
        .to_owned();
    let second = read_stream(addr, &format!("Last-Event-ID: {last_id}\r\n")).await;
    assert!(!second.contains("resync"), "{second}");
    assert!(second.contains("maximum age"), "{second}");
    // An id from before this run is a resync.
    let stale = read_stream(addr, "Last-Event-ID: 5\r\n").await;
    assert!(stale.contains("event: resync"), "{stale}");
    api.finish().await;
    csms.shutdown().await;
}
