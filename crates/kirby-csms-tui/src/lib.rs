//! Terminal UI for kirby-csms (ratatui + crossterm): observe many stations.
//!
//! The UI receives a [`CsmsHandle`] and reads the engine through `stations()`, `station()`,
//! `state()`, `history()` and `subscribe()`. The model ([`app`]) is pure: [`app::update`]
//! turns a [`app::Msg`] into [`app::Effect`]s and the run loop executes them. The terminal is
//! restored on every exit path (RAII guard + panic hook from `kirby-ocpp-tui`).
//!
//! Commands ([`command::Command`], opened from forms and the palette) run as background tasks:
//! the model emits [`app::Effect::Send`], [`execute`] spawns the call and its answer returns
//! as [`app::Msg::CommandDone`], so a slow station never blocks the UI.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "draw functions are long flat lists; ratatui code uses `f`, `w`, `h`"
)]

mod actions;
pub mod app;
pub mod command;
pub mod form;
mod keys;
pub mod ui;

use std::future::Future;
use std::io;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::{Stream, StreamExt};
use kirby_csms_core::CsmsHandle;
use kirby_csms_core::{Fault, Reply, Scope};
use kirby_ocpp_tui::clipboard;
use kirby_ocpp_tui::shutdown_signal;
use kirby_ocpp_tui::terminal::TerminalGuard;
use ratatui::Terminal;
use ratatui::backend::Backend;
use thiserror::Error;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use kirby_ocpp_tui::app::NoticeLevel;

use crate::actions::{Rules, ScopedRule};
use crate::app::{App, Effect, Msg, update};
use crate::form::FaultKind;

/// UI tuning knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    /// Minimum time between redraws (drawing only happens when something changed).
    pub tick_rate: Duration,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            tick_rate: Duration::from_millis(33),
        }
    }
}

/// Why the UI ended abnormally.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TuiError {
    /// The terminal could not be set up or drawn to.
    #[error("terminal error: {0}")]
    Terminal(#[from] io::Error),
}

/// The terminal taken over by the UI (raw mode, alternate screen). Dropping it restores the
/// terminal. Entering is separate from running so that a caller can fall back to a headless
/// mode when the terminal cannot be set up.
#[derive(Debug)]
pub struct Session {
    guard: TerminalGuard,
}

impl Session {
    pub fn enter() -> Result<Self, TuiError> {
        Ok(Self {
            guard: TerminalGuard::enter()?,
        })
    }

    /// Runs the UI until the user quits. Does not shut the server down; the caller does.
    pub async fn run(mut self, handle: CsmsHandle, options: TuiOptions) -> Result<(), TuiError> {
        let keys = EventStream::new();
        drive(
            self.guard.terminal(),
            &handle,
            &options,
            keys,
            shutdown_signal(),
            |_| {},
        )
        .await
    }
}

/// Enters the terminal and runs the UI until the user quits.
pub async fn run(handle: CsmsHandle, options: TuiOptions) -> Result<(), TuiError> {
    Session::enter()?.run(handle, options).await
}

/// Reads the whole registry: infos and the state of every station.
fn resync_msg(handle: &CsmsHandle) -> Msg {
    let infos = handle.stations();
    let states = infos
        .iter()
        .map(|i| (i.id.clone(), handle.state(&i.id).map(Box::new)))
        .collect();
    Msg::Resynced { infos, states }
}

/// Where effects that take time run, and where their answers arrive.
#[derive(Debug)]
pub(crate) struct Exec {
    tx: mpsc::UnboundedSender<Msg>,
    tasks: JoinSet<()>,
}

impl Exec {
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                tasks: JoinSet::new(),
            },
            rx,
        )
    }

    /// Forgets the tasks that have finished.
    fn reap(&mut self) {
        while self.tasks.try_join_next().is_some() {}
    }
}

fn rules_msg(handle: &CsmsHandle) -> Msg {
    Msg::Rules(Rules {
        actions: handle.responder_actions(),
        scoped: handle
            .scoped_rules(None)
            .into_iter()
            .map(|rule| ScopedRule {
                station: rule.scope.station,
                action: rule.action,
            })
            .collect(),
        faults: handle.faults_remaining(),
    })
}

fn fault_for(action: &str, kind: FaultKind, times: usize, payload: serde_json::Value) -> Fault {
    match kind {
        FaultKind::CallError => Fault::call_error(action),
        FaultKind::Silence => Fault::silence(action),
        FaultKind::Close => Fault::close(action),
        FaultKind::InvalidPayload => Fault::invalid_payload(action, payload),
    }
    .times(times)
}

/// Performs one effect; the messages it produces are fed back into [`update`] at once. A
/// command is only spawned here; its answer arrives later through the [`Exec`] channel.
async fn execute(effect: Effect, handle: &CsmsHandle, exec: &mut Exec) -> Vec<Msg> {
    match effect {
        Effect::Send { station, command } => {
            let (handle, tx) = (handle.clone(), exec.tx.clone());
            exec.tasks.spawn(async move {
                let outcome = command::run(&handle, &station, &command).await;
                let _ = tx.send(Msg::CommandDone {
                    station,
                    command,
                    outcome,
                });
            });
            vec![]
        }
        Effect::Respond {
            action,
            payload,
            station,
        } => match station {
            None => {
                let reply = Reply::Result(payload);
                handle.respond(&action, move |_| reply.clone());
                vec![rules_msg(handle)]
            }
            Some(station) => {
                match handle.respond_scoped(&Scope::station(station), &action, payload) {
                    Ok(()) => vec![rules_msg(handle)],
                    Err(error) => vec![Msg::Notice(NoticeLevel::Error, error), rules_msg(handle)],
                }
            }
        },
        Effect::Inject {
            action,
            kind,
            times,
            payload,
            station,
        } => {
            let scope = station.map_or_else(Scope::global, Scope::station);
            let _ = handle.inject_scoped(fault_for(&action, kind, times, payload), scope);
            vec![rules_msg(handle)]
        }
        Effect::RemoveRule(action) => {
            handle.clear_responder(&action);
            vec![rules_msg(handle)]
        }
        Effect::RemoveScopedRule { station, action } => {
            handle.clear_scoped_rule(station.as_deref(), &action);
            vec![rules_msg(handle)]
        }
        Effect::ClearRules => {
            handle.clear_faults();
            for action in handle.responder_actions() {
                handle.clear_responder(&action);
            }
            handle.clear_scoped_rules();
            vec![rules_msg(handle)]
        }
        Effect::ReadRules => vec![rules_msg(handle)],
        Effect::Forget(id) => {
            let text = if handle.forget(&id) {
                format!("forgot {}", kirby_ocpp_tui::text::sanitize(&id))
            } else {
                "station already gone".to_owned()
            };
            vec![Msg::Notice(kirby_ocpp_tui::app::NoticeLevel::Success, text)]
        }
        Effect::Load(id) => vec![Msg::Loaded {
            history: handle.history(&id).unwrap_or_default(),
            info: handle.station(&id).map(Box::new),
            state: handle.state(&id).map(Box::new),
            id,
        }],
        Effect::Refresh { states, info } => vec![Msg::Refreshed {
            states: states
                .into_iter()
                .map(|id| {
                    let state = handle.state(&id).map(Box::new);
                    (id, state)
                })
                .collect(),
            info: info.map(|id| {
                let info = handle.station(&id).map(Box::new);
                (id, info)
            }),
        }],
        Effect::Resync => vec![resync_msg(handle)],
        Effect::Copy { text, what } => {
            let (level, text) =
                clipboard::copy_with_fallback(&text, &what, &mut io::stdout()).await;
            vec![Msg::Notice(level, text)]
        }
    }
}

/// The run loop over any terminal backend and key stream. `observe` sees the model after every
/// message (tests use it to decide when to quit).
pub(crate) async fn drive<B, S>(
    terminal: &mut Terminal<B>,
    handle: &CsmsHandle,
    options: &TuiOptions,
    mut keys: S,
    signal: impl Future<Output = ()>,
    mut observe: impl FnMut(&App),
) -> Result<(), TuiError>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    S: Stream<Item = io::Result<TermEvent>> + Unpin,
{
    // Subscribe before reading the registry: nothing happens in between that we would miss.
    let mut events = handle.subscribe();
    let mut events_open = true;
    let mut app = App::new(handle.url(), Utc::now());
    app.security_profile = handle.security_profile().number();
    if let Ok(size) = terminal.size() {
        app.size = (size.width, size.height);
    }
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(signal);

    let (mut exec, mut done) = Exec::new();
    // FIFO: messages produced by effects must be handled in the order the effects ran.
    let mut queue = std::collections::VecDeque::from([resync_msg(handle)]);
    while !app.quit {
        let msg = if let Some(msg) = queue.pop_front() {
            msg
        } else {
            tokio::select! {
                ev = keys.next() => match ev {
                    Some(Ok(TermEvent::Key(k))) => Msg::Key(k),
                    Some(Ok(TermEvent::Paste(s))) => Msg::Paste(s),
                    Some(Ok(TermEvent::Resize(w, h))) => Msg::Resize(w, h),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                ev = events.recv(), if events_open => match ev {
                    Ok(event) => Msg::Event(event),
                    Err(RecvError::Lagged(n)) => Msg::Lagged(n),
                    Err(RecvError::Closed) => {
                        events_open = false;
                        Msg::Closed
                    }
                },
                Some(msg) = done.recv() => msg,
                _ = tick.tick() => {
                    exec.reap();
                    Msg::Tick(Utc::now())
                }
                () = &mut signal => Msg::Signal,
            }
        };
        let is_tick = matches!(msg, Msg::Tick(_));
        for effect in update(&mut app, msg) {
            queue.extend(execute(effect, handle, &mut exec).await);
        }
        observe(&app);
        if is_tick && app.dirty {
            draw(terminal, &mut app)?;
        }
    }
    if app.dirty {
        draw(terminal, &mut app)?;
    }
    Ok(())
}

fn draw<B>(terminal: &mut Terminal<B>, app: &mut App) -> Result<(), TuiError>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal
        .draw(|f| ui::draw(f, app))
        .map_err(io::Error::other)?;
    app.dirty = false;
    Ok(())
}

#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod list_forms_tests;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod tests;
