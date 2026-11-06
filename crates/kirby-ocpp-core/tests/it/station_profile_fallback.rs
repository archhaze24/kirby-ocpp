//! Security Whitepaper `A05.FR.08`: after a change of `SecurityProfile` the station goes back to
//! the previous profile when the Central System cannot be reached with the new one.
//!
//! The mock CSMS has no authentication, so a small gate stands in front of it: it forwards
//! upgrade requests without `Authorization` (profile 0) and, depending on its mode, answers
//! requests with Basic Auth (profile 1) with `401` or holds them until the station times out.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::station_support::{Rig, STEP, t0, wait_state};
use kirby_ocpp_core::Station;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_testkit::{CsmsOptions, Reply};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FORWARD: u8 = 0;
const REFUSE: u8 = 1;
const SILENT: u8 = 2;

struct Gate {
    url: String,
    mode: Arc<AtomicU8>,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    refused: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
}

impl Gate {
    async fn start(mode: u8) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gate = Self {
            url: format!(
                "ws://127.0.0.1:{}/ocpp",
                listener.local_addr().unwrap().port()
            ),
            mode: Arc::new(AtomicU8::new(mode)),
            upstream: Arc::default(),
            refused: Arc::default(),
            held: Arc::default(),
        };
        let (mode, upstream) = (Arc::clone(&gate.mode), Arc::clone(&gate.upstream));
        let (refused, held) = (Arc::clone(&gate.refused), Arc::clone(&gate.held));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (mode, upstream) = (Arc::clone(&mode), Arc::clone(&upstream));
                let (refused, held) = (Arc::clone(&refused), Arc::clone(&held));
                tokio::spawn(async move {
                    serve(socket, mode, upstream, refused, held).await;
                });
            }
        });
        gate
    }

    fn set_mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn set_upstream(&self, addr: SocketAddr) {
        *self.upstream.lock().unwrap() = Some(addr);
    }

    fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }

    fn held(&self) -> usize {
        self.held.load(Ordering::SeqCst)
    }
}

async fn serve(
    mut socket: TcpStream,
    mode: Arc<AtomicU8>,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    refused: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
) {
    let mut request = Vec::new();
    let mut buf = [0_u8; 2048];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let authenticated = String::from_utf8_lossy(&request)
        .to_ascii_lowercase()
        .contains("\r\nauthorization:");
    let mode = mode.load(Ordering::SeqCst);
    if authenticated && mode == REFUSE {
        refused.fetch_add(1, Ordering::SeqCst);
        let _ = socket
            .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
            .await;
        return;
    }
    if authenticated && mode == SILENT {
        held.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        return;
    }
    let Some(addr) = *upstream.lock().unwrap() else {
        return;
    };
    let Ok(mut server) = TcpStream::connect(addr).await else {
        return;
    };
    if server.write_all(&request).await.is_ok() {
        let _ = tokio::io::copy_bidirectional(&mut socket, &mut server).await;
    }
}

async fn rig(gate: &Gate, fallback: bool) -> Rig {
    let url = gate.url.clone();
    let mut rig = Rig::with(CsmsOptions::default(), move |mut config| {
        config.url = url;
        config.call_timeout_ms = 300;
        config.security_profile_fallback = fallback;
        config
    })
    .await;
    gate.set_upstream(rig.csms.addr());
    rig.csms
        .respond("SecurityEventNotification", |_| Reply::Result(json!({})));
    rig.connect_booted().await;
    rig
}

/// Sets the key and the profile through the CSMS; the station reconnects after each.
async fn switch_to_profile_one(rig: &Rig) {
    rig.cfg("AuthorizationKey", "0123456789abcdef").await;
    rig.csms
        .wait_for_connection_count(2, STEP)
        .await
        .expect("reconnect after the key");
    rig.wait_registration(RegistrationStatus::Accepted).await;
    rig.cfg("SecurityProfile", "1").await;
}

fn state_file(rig: &Rig) -> Value {
    let name = format!("{}.station.json", rig.station.config().charge_point_id);
    serde_json::from_slice(&std::fs::read(rig.dir.path().join(name)).unwrap()).unwrap()
}

async fn fell_back_event(rig: &Rig) {
    rig.csms
        .wait_for_call_after(
            0,
            "SecurityEventNotification",
            |p| {
                p["type"] == "ReconfigurationOfSecurityParameters"
                    && p["techInfo"]
                        .as_str()
                        .is_some_and(|t| t.contains("fell back from 1 to 0"))
            },
            STEP,
        )
        .await
        .expect("the fallback is reported");
}

#[tokio::test]
async fn refused_new_profile_falls_back_to_the_previous_one() {
    let gate = Gate::start(REFUSE).await;
    let mut rig = rig(&gate, true).await;
    switch_to_profile_one(&rig).await;
    wait_state(&rig.station, "fallback to profile 0", |s| {
        s.security_profile == 0
    })
    .await;
    rig.wait_registration(RegistrationStatus::Accepted).await;
    assert_eq!(
        gate.refused(),
        3,
        "three counted failures, then the fallback"
    );
    fell_back_event(&rig).await;
    rig.station.shutdown().await.unwrap();
    assert!(state_file(&rig)["pending_profile_switch"].is_null());
    assert_eq!(state_file(&rig)["configuration"]["SecurityProfile"], "0");
    rig.join.take().unwrap().await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn a_connection_with_the_new_profile_ends_the_fallback_window() {
    let gate = Gate::start(FORWARD).await;
    let rig = rig(&gate, true).await;
    switch_to_profile_one(&rig).await;
    rig.csms
        .wait_for_connection_count(3, STEP)
        .await
        .expect("reconnect with profile 1");
    rig.wait_registration(RegistrationStatus::Accepted).await;
    // The settled switch is dropped from the persisted state (flushed with the boot).
    rig.station.shutdown().await.unwrap();
    assert!(state_file(&rig)["pending_profile_switch"].is_null());
    let (station, join) = Station::spawn(
        rig.station.config().clone(),
        Arc::new(kirby_ocpp_core::ManualClock::new(t0())),
    );
    // Later refusals never lower the profile (A05.FR.09).
    gate.set_mode(REFUSE);
    station.connect().await.unwrap();
    wait_state(&station, "five refusals", |_| gate.refused() >= 5).await;
    assert_eq!(station.snapshot().security_profile, 1);
    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn timeouts_never_fall_back() {
    let gate = Gate::start(SILENT).await;
    let mut rig = rig(&gate, true).await;
    switch_to_profile_one(&rig).await;
    wait_state(&rig.station, "five timed out attempts", |_| {
        gate.held() >= 5
    })
    .await;
    assert_eq!(rig.station.snapshot().security_profile, 1);
    rig.station.shutdown().await.unwrap();
    assert_eq!(
        state_file(&rig)["pending_profile_switch"]["previous_profile"],
        0
    );
    assert_eq!(state_file(&rig)["pending_profile_switch"]["failures"], 0);
    rig.join.take().unwrap().await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn the_pending_switch_survives_a_restart_and_still_falls_back() {
    let gate = Gate::start(SILENT).await;
    let mut rig = rig(&gate, true).await;
    switch_to_profile_one(&rig).await;
    wait_state(&rig.station, "a held attempt", |_| gate.held() >= 1).await;
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    assert_eq!(state_file(&rig)["version"], 9);
    assert_eq!(
        state_file(&rig)["pending_profile_switch"]["previous_profile"],
        0
    );

    gate.set_mode(REFUSE);
    let (station, join) = Station::spawn(
        rig.station.config().clone(),
        Arc::new(kirby_ocpp_core::ManualClock::new(t0())),
    );
    assert_eq!(station.snapshot().security_profile, 1);
    station.connect().await.unwrap();
    wait_state(&station, "fallback after the restart", |s| {
        s.security_profile == 0 && s.registration == Some(RegistrationStatus::Accepted)
    })
    .await;
    station.shutdown().await.unwrap();
    join.await.unwrap();
    assert!(state_file(&rig)["pending_profile_switch"].is_null());
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn with_the_flag_off_the_station_stays_on_the_new_profile() {
    let gate = Gate::start(REFUSE).await;
    let mut rig = rig(&gate, false).await;
    switch_to_profile_one(&rig).await;
    wait_state(&rig.station, "five refusals", |_| gate.refused() >= 5).await;
    assert_eq!(rig.station.snapshot().security_profile, 1);
    rig.station.shutdown().await.unwrap();
    assert!(state_file(&rig)["pending_profile_switch"].is_null());
    rig.join.take().unwrap().await.unwrap();
    rig.csms.shutdown().await;
}
