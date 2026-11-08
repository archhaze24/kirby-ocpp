//! Terminal UI for kirby-ocpp (ratatui + crossterm).
//!
//! The UI receives a [`StationHandle`] and talks to the station only through
//! `send(Command)`, `snapshot()` and `subscribe()`. The model ([`app`]) is pure:
//! [`app::update`] turns a [`app::Msg`] into [`app::Effect`]s, and [`run`] executes them.
//! The terminal is restored on every exit path (RAII guard + panic hook).

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "catalogues and draw functions are long flat lists; ratatui code uses `f`, `w`, `h`"
)]

pub mod actions;
pub mod app;
pub mod browser;
pub mod clipboard;
pub mod fleet;
pub mod forms;
mod keys;
pub mod logview;
pub mod palette;
pub mod stdout_pump;
pub mod terminal;
#[cfg(test)]
mod tests_support;
pub mod text;
pub mod theme;
pub mod ui;

use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use kirby_ocpp_core::StationHandle;
use kirby_ocpp_core::scenario::{CancelToken, Scenario, ScenarioRunner};
use kirby_ocpp_core::station::Command;
use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::timefmt::TimeMode;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};

use crate::app::{App, Effect, Msg, NoticeLevel};
use crate::fleet::{FleetApp, FleetEffect, FleetMsg, update_fleet};

/// UI tuning knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    /// Minimum time between redraws (drawing only happens when something changed).
    pub tick_rate: Duration,
    /// Draw without color (see [`theme`]).
    pub monochrome: bool,
    /// Clock of the log rows.
    pub time: TimeMode,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            tick_rate: Duration::from_millis(33),
            monochrome: false,
            time: TimeMode::Utc,
        }
    }
}

/// Why the UI ended abnormally.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TuiError {
    /// The terminal could not be set up or drawn to.
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
}

/// Resolves on SIGINT or SIGTERM (Ctrl-C only on non-Unix).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs the UI for one station until the user quits. Does not shut the station down; the
/// caller does.
pub async fn run(handle: StationHandle, options: TuiOptions) -> Result<(), TuiError> {
    run_fleet(&[handle], options).await
}

/// What the loop receives from the station tasks.
enum Inbound {
    Msg(usize, Msg),
    /// The station's event channel closed (the station ended).
    Gone,
}

/// Sends messages for one station into the loop.
#[derive(Clone)]
struct Out {
    tx: mpsc::UnboundedSender<Inbound>,
    station: usize,
}

impl Out {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(Inbound::Msg(self.station, msg));
    }
}

/// What the loop needs per station besides the model.
struct Ctx {
    handle: StationHandle,
    runner: ScenarioRunner,
    cancel: CancelToken,
    out: Out,
}

/// Station events that may wait for the UI loop before new ones are dropped.
const EVENT_QUEUE: usize = 8192;

/// Pushes one forwarded station message into the bounded queue; a full queue drops it and
/// counts it in `dropped`.
fn deliver(tx: &mpsc::Sender<Inbound>, station: usize, msg: Msg, dropped: &mut u64) {
    if tx.try_send(Inbound::Msg(station, msg)).is_err() {
        *dropped += 1;
    }
}

/// After drops, once the queue has room again: a fresh snapshot (the UI resyncs from it) and
/// one log line with the count. Keeps `dropped` while there is no room.
fn resync(
    tx: &mpsc::Sender<Inbound>,
    station: usize,
    dropped: &mut u64,
    snapshot: impl FnOnce() -> Snapshot,
) {
    if *dropped == 0 {
        return;
    }
    let (Ok(first), Ok(second)) = (tx.try_reserve(), tx.try_reserve()) else {
        return;
    };
    first.send(Inbound::Msg(station, Msg::Snapshot(Box::new(snapshot()))));
    second.send(Inbound::Msg(
        station,
        Msg::Notice(
            NoticeLevel::Error,
            format!("UI too slow: {dropped} events dropped, state resynced (the log has a gap)"),
        ),
    ));
    *dropped = 0;
}

/// Forwards one station's events and snapshots into the loop through a bounded queue, so a
/// stalled draw cannot make memory grow.
fn forward_station(handle: &StationHandle, station: usize, tx: &mpsc::Sender<Inbound>) {
    let mut events = handle.subscribe();
    let mut snapshots = handle.watch_snapshot();
    let (handle, tx) = (handle.clone(), tx.clone());
    tokio::spawn(async move {
        let mut dropped = 0u64;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(e) => deliver(&tx, station, Msg::Event(e), &mut dropped),
                    Err(broadcast::error::RecvError::Lagged(n)) => dropped += n,
                    Err(broadcast::error::RecvError::Closed) => {
                        let _ = tx.send(Inbound::Gone).await;
                        return;
                    }
                },
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        let _ = tx.send(Inbound::Gone).await;
                        return;
                    }
                    let snapshot = Msg::Snapshot(Box::new(snapshots.borrow_and_update().clone()));
                    deliver(&tx, station, snapshot, &mut dropped);
                }
                // Retries the resync while nothing else happens.
                () = tokio::time::sleep(Duration::from_millis(100)), if dropped > 0 => {}
            }
            resync(&tx, station, &mut dropped, || handle.snapshot());
        }
    });
}

/// Runs the UI over one or more stations until the user quits. With more than one station
/// a selector row is shown (see [`fleet`]). Does not shut the stations down; the caller does.
pub async fn run_fleet(stations: &[StationHandle], options: TuiOptions) -> Result<(), TuiError> {
    let mut guard = terminal::TerminalGuard::enter()?;
    let apps: Vec<App> = stations
        .iter()
        .map(|handle| {
            let config = handle.config();
            App::new(
                handle.snapshot(),
                text::redact_url(&config.url),
                config.vendor.clone(),
            )
        })
        .collect();
    let mut model = FleetApp::new(apps);
    for app in &mut model.apps {
        app.mono = options.monochrome;
        app.time = options.time;
    }
    if let Ok(size) = guard.terminal().size() {
        for app in &mut model.apps {
            app.size = (size.width, size.height);
        }
    }

    // User-action results (few, never dropped) and forwarded station events (bounded).
    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<Inbound>();
    let (event_tx, mut event_rx) = mpsc::channel::<Inbound>(EVENT_QUEUE);
    let mut ctxs: Vec<Ctx> = Vec::with_capacity(stations.len());
    for (station, handle) in stations.iter().enumerate() {
        let out = Out {
            tx: user_tx.clone(),
            station,
        };
        forward_station(handle, station, &event_tx);
        ctxs.push(Ctx {
            handle: handle.clone(),
            runner: ScenarioRunner::new(handle.clone()),
            cancel: CancelToken::new(),
            out,
        });
    }
    let mut gone = 0;
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let signal = shutdown_signal();
    tokio::pin!(signal);

    while !model.quit {
        let msg = tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(TermEvent::Key(k))) => FleetMsg::Key(k),
                Some(Ok(TermEvent::Paste(s))) => FleetMsg::Paste(s),
                Some(Ok(TermEvent::Resize(w, h))) => FleetMsg::Resize(w, h),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(inbound) = async {
                tokio::select! {
                    m = user_rx.recv() => m,
                    m = event_rx.recv() => m,
                }
            } => match inbound {
                Inbound::Msg(station, msg) => FleetMsg::Station { station, msg },
                Inbound::Gone => {
                    gone += 1;
                    if gone >= ctxs.len() {
                        break;
                    }
                    continue;
                }
            },
            _ = tick.tick() => FleetMsg::Tick,
            () = &mut signal => FleetMsg::Signal,
        };
        let is_tick = matches!(msg, FleetMsg::Tick);
        for effect in update_fleet(&mut model, msg) {
            match effect {
                FleetEffect::Station { station, effect } => {
                    if let Some(ctx) = ctxs.get_mut(station) {
                        execute(effect, ctx, &mut guard).await;
                    }
                }
                FleetEffect::ConnectAll => {
                    for ctx in &ctxs {
                        let h = ctx.handle.clone();
                        let out = ctx.out.clone();
                        tokio::spawn(async move {
                            if let Err(e) = h.connect().await {
                                out.send(Msg::Notice(
                                    NoticeLevel::Error,
                                    format!("connect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
                FleetEffect::DisconnectAll => {
                    for ctx in &ctxs {
                        let h = ctx.handle.clone();
                        let out = ctx.out.clone();
                        tokio::spawn(async move {
                            if let Err(e) = h.disconnect().await {
                                out.send(Msg::Notice(
                                    NoticeLevel::Error,
                                    format!("disconnect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
            }
        }
        if is_tick && model.dirty {
            guard.terminal().draw(|f| ui::draw_fleet(f, &model))?;
            model.dirty = false;
            model.current_mut().dirty = false;
        }
    }
    for ctx in &ctxs {
        ctx.cancel.cancel();
    }
    Ok(())
}

async fn execute(effect: Effect, ctx: &mut Ctx, guard: &mut terminal::TerminalGuard) {
    match effect {
        Effect::Send {
            label,
            command,
            show_response,
        } => {
            let (h, out) = (ctx.handle.clone(), ctx.out.clone());
            tokio::spawn(async move {
                let result = h.send(command).await.map_err(|e| e.to_string());
                out.send(Msg::CommandDone {
                    label,
                    show_response,
                    result,
                });
            });
        }
        Effect::CancelCall { action } => {
            let (h, out) = (ctx.handle.clone(), ctx.out.clone());
            tokio::spawn(async move {
                // The waiting command completes with its own error; only a refusal needs a
                // word here (nothing left to cancel, or a transaction message).
                if let Err(e) = h.send(Command::CancelCall { action }).await {
                    out.send(Msg::Notice(NoticeLevel::Error, e.to_string()));
                }
            });
        }
        Effect::RunScenario(scenario) => {
            ctx.cancel = CancelToken::new();
            spawn_scenario(
                *scenario,
                ctx.runner.clone(),
                ctx.out.clone(),
                ctx.cancel.clone(),
            );
        }
        Effect::RunScenarioFile(path) => {
            ctx.cancel = CancelToken::new();
            let (runner, out, cancel) = (ctx.runner.clone(), ctx.out.clone(), ctx.cancel.clone());
            load_scenario_in_background(path, ctx.out.clone(), move |scenario| {
                spawn_scenario(scenario, runner, out, cancel);
            });
        }
        Effect::CancelScenario => ctx.cancel.cancel(),
        Effect::Copy { text, what } => {
            let (level, text) =
                clipboard::copy_with_fallback(&text, &what, guard.terminal().backend_mut()).await;
            ctx.out.send(Msg::Notice(level, text));
        }
        Effect::WriteFile { path, contents } => {
            let notice = match write_private(&path, contents).await {
                Ok(()) => (NoticeLevel::Success, format!("log exported to {path}")),
                Err(e) => (NoticeLevel::Error, format!("export to {path} failed: {e}")),
            };
            ctx.out.send(Msg::Notice(notice.0, notice.1));
        }
    }
}

/// Writes the export readable by the owner only: logs contain idTags and meter data. Refuses
/// a symlink, and tightens the mode of an existing file before any byte is written.
async fn write_private(path: &str, contents: String) -> std::io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        write_private_blocking(std::path::Path::new(&path), &contents)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn write_private_blocking(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other("refusing to write through a symlink"));
    }
    let mut options = std::fs::OpenOptions::new();
    // No truncation yet: an existing file is made private first.
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if file.metadata()?.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.set_len(0)?;
    }
    file.write_all(contents.as_bytes())
}

/// Loads a scenario file off the UI loop (a FIFO or a stalled mount must not freeze keys,
/// ticks or shutdown); a failure goes back as `Msg::ScenarioError`, a success to `start`.
fn load_scenario_in_background(
    path: String,
    out: Out,
    start: impl FnOnce(Scenario) + Send + 'static,
) {
    tokio::spawn(async move {
        match tokio::task::spawn_blocking(move || Scenario::load(std::path::Path::new(&path))).await
        {
            Ok(Ok(scenario)) => start(scenario),
            Ok(Err(e)) => out.send(Msg::ScenarioError(e.to_string())),
            Err(e) => out.send(Msg::ScenarioError(e.to_string())),
        }
    });
}

fn spawn_scenario(scenario: Scenario, runner: ScenarioRunner, out: Out, cancel: CancelToken) {
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let relay = out.clone();
    tokio::spawn(async move {
        while let Some(p) = progress_rx.recv().await {
            relay.send(Msg::Scenario(Box::new(p)));
        }
    });
    tokio::spawn(async move {
        let msg = match runner.run(&scenario, &cancel, Some(&progress_tx)).await {
            Ok(report) => Msg::ScenarioDone(Box::new(report)),
            Err(e) => Msg::ScenarioError(e.to_string()),
        };
        drop(progress_tx);
        out.send(msg);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tick_rate_is_about_30_fps() {
        assert_eq!(TuiOptions::default().tick_rate, Duration::from_millis(33));
    }

    fn notice() -> Msg {
        Msg::Notice(NoticeLevel::Info, "x".to_owned())
    }

    #[test]
    fn full_queue_drops_counts_and_resyncs() {
        let (tx, mut rx) = mpsc::channel::<Inbound>(4);
        let mut dropped = 0;
        for _ in 0..10 {
            deliver(&tx, 0, notice(), &mut dropped);
        }
        assert_eq!(dropped, 6);
        let snapshot = || Snapshot::initial(&kirby_ocpp_core::StationConfig::new("ws://h/p"));
        // No room: nothing is sent and the count stays.
        resync(&tx, 0, &mut dropped, snapshot);
        assert_eq!(dropped, 6);
        while rx.try_recv().is_ok() {}
        resync(&tx, 0, &mut dropped, snapshot);
        assert_eq!(dropped, 0);
        assert!(matches!(
            rx.try_recv(),
            Ok(Inbound::Msg(0, Msg::Snapshot(_)))
        ));
        match rx.try_recv() {
            Ok(Inbound::Msg(0, Msg::Notice(NoticeLevel::Error, text))) => {
                assert!(text.contains("6 events dropped"), "{text}");
            }
            _ => panic!("expected the drop notice"),
        }
        // Nothing dropped: nothing sent.
        resync(&tx, 0, &mut dropped, snapshot);
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn export_tightens_an_existing_file_and_refuses_symlinks() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log.txt");
        std::fs::write(&file, "old old old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_blocking(&file, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(write_private_blocking(&link, "pwned").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let dangling = dir.path().join("dangling.txt");
        std::os::unix::fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(write_private_blocking(&dangling, "x").is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scenario_file_load_does_not_block_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("scenario.toml");
        // `unsafe` is forbidden, so use the mkfifo binary.
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let out = Out { tx, station: 0 };
        // Returns at once, and the loader refuses a FIFO without ever opening it (a blocking
        // open would hang a worker thread and the runtime teardown), so the error comes back.
        load_scenario_in_background(fifo.to_string_lossy().into_owned(), out, |_| {});
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Some(Inbound::Msg(0, Msg::ScenarioError(_)))) => {}
            _ => panic!("expected a ScenarioError"),
        }
    }
}
