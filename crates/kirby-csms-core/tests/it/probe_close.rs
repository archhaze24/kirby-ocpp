//! The `close` step of the connection probe against the real engine and against hand-written
//! servers that end the connection in other ways.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{CsmsConfig, CsmsServer};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ws::probe::{self, ProbeOptions, Report, Step, StepStatus};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

/// Generous bound on a whole probe run in these tests.
const RUN_LIMIT: Duration = Duration::from_secs(30);

/// How a hand-written server ends the connection after answering `BootNotification`.
#[derive(Clone, Copy)]
enum Ending {
    /// Never reads again, so a Close from the probe is never answered.
    Hold,
    /// Reads the Close and drops the socket without a reply.
    DropOnClose,
}

async fn probe_url(url: String, timeout: Duration) -> (Report, Duration) {
    let config = StationConfig::new(url).with_charge_point_id("CP-PROBE");
    let options = ProbeOptions::from_config(&config, timeout);
    let started = Instant::now();
    let report = tokio::time::timeout(RUN_LIMIT, probe::run(&options, &mut |_: &Step| {}))
        .await
        .expect("the probe finishes");
    (report, started.elapsed())
}

fn close_step(report: &Report) -> &Step {
    report
        .steps
        .iter()
        .find(|step| step.name == "close")
        .expect("a close step")
}

/// A server that answers the boot after `delay` and then ends as `ending` says.
async fn scripted_server(delay: Duration, ending: Ending) -> (String, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ocpp", listener.local_addr().unwrap());
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let Ok(Ok((stream, _))) = timeout(RUN_LIMIT, listener.accept()).await else {
            return;
        };
        #[allow(
            clippy::result_large_err,
            reason = "the error type is fixed by tungstenite's `Callback` trait"
        )]
        let callback = |request: &Request, mut response: Response| {
            if let Some(protocol) = request.headers().get("Sec-WebSocket-Protocol") {
                response
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", protocol.clone());
            }
            Ok(response)
        };
        let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
            return;
        };
        while let Some(Ok(message)) = socket.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let frame: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            let id = frame[1].as_str().unwrap().to_owned();
            tokio::time::sleep(delay).await;
            let answer = serde_json::json!([
                3,
                id,
                {"status": "Accepted", "currentTime": "2026-01-01T00:00:00Z", "interval": 30}
            ]);
            socket
                .send(Message::text(answer.to_string()))
                .await
                .unwrap();
            break;
        }
        match ending {
            Ending::Hold => {
                let _ = stop_rx.await;
            }
            Ending::DropOnClose => {
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                }
                drop(socket);
            }
        }
    });
    (url, stop_tx)
}

#[tokio::test]
async fn a_server_that_echoes_the_close_passes() {
    let csms = CsmsServer::start(CsmsConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("engine starts");
    let (report, _) = probe_url(format!("{}/ocpp", csms.url()), Duration::from_secs(20)).await;
    let step = close_step(&report);
    assert_eq!(step.status, StepStatus::Passed, "{}", step.detail);
    assert!(step.detail.contains("code 1000"), "{}", step.detail);
    csms.shutdown().await;
}

#[tokio::test]
async fn a_server_that_drops_the_socket_without_a_close_reply_warns() {
    let (url, _stop) = scripted_server(Duration::ZERO, Ending::DropOnClose).await;
    let (report, _) = probe_url(url, Duration::from_secs(20)).await;
    let step = close_step(&report);
    assert_eq!(step.status, StepStatus::Warning, "{}", step.detail);
    assert!(
        step.detail.contains("without a close reply"),
        "{}",
        step.detail
    );
}

#[tokio::test]
async fn the_close_wait_is_bounded_by_the_overall_timeout() {
    // The boot answer takes most of the 2 s budget and the server never answers the close.
    let (url, _stop) = scripted_server(Duration::from_millis(1_500), Ending::Hold).await;
    let (report, elapsed) = probe_url(url, Duration::from_secs(2)).await;
    let step = close_step(&report);
    assert_eq!(step.status, StepStatus::Warning, "{}", step.detail);
    // The fixed close wait is 5 s: without the bound the run needs about 6.5 s.
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}
