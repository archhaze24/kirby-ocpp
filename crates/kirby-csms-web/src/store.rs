//! The state every screen shares: the station list, the live frame ring, the token and the
//! event stream with its reconnect loop.

use std::cell::RefCell;
use std::collections::VecDeque;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use web_sys::AbortController;

use crate::api::{self, ApiError, Frame, Health, Station};
use crate::util;

const TOKEN_KEY: &str = "kirby-csms-token";
/// Frames kept in the browser; older ones are dropped (the history endpoint has more).
pub const FRAME_CAP: usize = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Open,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Log,
    Commands,
    Rules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Stations,
    Transactions,
}

/// A frame with a number that never repeats while the page lives (a stable list key).
#[derive(Debug, Clone, PartialEq)]
pub struct LogFrame {
    pub seq: u64,
    pub frame: Frame,
}

#[derive(Debug, Clone, Copy)]
pub struct Store {
    pub token: RwSignal<Option<String>>,
    pub need_token: RwSignal<bool>,
    pub stations: RwSignal<Vec<Station>>,
    pub version: RwSignal<String>,
    pub live: RwSignal<Link>,
    pub frames: RwSignal<VecDeque<LogFrame>>,
    pub selected: RwSignal<Option<String>>,
    pub view: RwSignal<View>,
    pub tab: RwSignal<Tab>,
    /// Grows whenever a station connected, disconnected, changed state or was forgotten:
    /// screens that show server data read it to know when to load again.
    pub tick: RwSignal<u64>,
    /// Whether the station drawer is open (narrow screens).
    pub drawer: RwSignal<bool>,
    seq: StoredValue<u64>,
    tick_pending: StoredValue<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StationEvent {
    station: Option<String>,
}

impl Store {
    pub fn new() -> Self {
        let token = util::stored(true, TOKEN_KEY).filter(|token| !token.is_empty());
        Self {
            token: RwSignal::new(token),
            need_token: RwSignal::new(false),
            stations: RwSignal::new(Vec::new()),
            version: RwSignal::new(String::new()),
            live: RwSignal::new(Link::Connecting),
            frames: RwSignal::new(VecDeque::new()),
            selected: RwSignal::new(None),
            view: RwSignal::new(View::Stations),
            tab: RwSignal::new(Tab::Overview),
            tick: RwSignal::new(0),
            drawer: RwSignal::new(false),
            seq: StoredValue::new(0),
            tick_pending: StoredValue::new(false),
        }
    }

    /// Uses `token` for every request from now on (`None` forgets it).
    pub fn set_token(&self, token: Option<String>) {
        util::store(true, TOKEN_KEY, token.as_deref());
        self.token.set(token);
        self.need_token.set(false);
        self.bump();
    }

    pub fn bump(&self) {
        self.tick.update(|tick| *tick += 1);
    }

    /// `bump`, but at most once in 150 ms however many events arrive.
    fn bump_soon(self) {
        if self.tick_pending.get_value() {
            return;
        }
        self.tick_pending.set_value(true);
        spawn_local(async move {
            util::sleep(150).await;
            self.tick_pending.set_value(false);
            self.bump();
        });
    }

    fn check<T>(&self, result: &Result<T, ApiError>) {
        if matches!(result, Err(ApiError::Unauthorized)) {
            self.need_token.set(true);
        }
    }

    /// `GET path` as JSON; a `401` opens the token prompt.
    pub async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, ApiError> {
        let result =
            api::request_json("GET", path, None, self.token.get_untracked().as_deref()).await;
        self.check(&result);
        result
    }

    /// A request with an optional JSON body; returns the answer text.
    pub async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, ApiError> {
        let result = api::request(method, path, body, self.token.get_untracked().as_deref()).await;
        self.check(&result);
        result
    }

    /// Reloads the station list and the version.
    pub async fn refresh_stations(&self) {
        if let Ok(list) = self.get::<StationList>("/v1/stations").await {
            let mut stations = list.stations;
            stations.sort_by(|a, b| a.id.cmp(&b.id));
            self.stations.update(|current| {
                if *current != stations {
                    *current = stations;
                }
            });
        }
        if let Ok(health) = self.get::<Health>("/v1/health").await {
            self.version.set(health.version);
        }
    }

    pub fn station(&self, id: &str) -> Option<Station> {
        self.stations
            .with(|stations| stations.iter().find(|s| s.id == id).cloned())
    }

    fn push_frame(&self, frame: Frame) {
        let seq = self.seq.get_value();
        self.seq.set_value(seq + 1);
        self.frames.update(|frames| {
            frames.push_back(LogFrame { seq, frame });
            while frames.len() > FRAME_CAP {
                frames.pop_front();
            }
        });
    }

    /// Adds frames loaded from the history that the ring does not hold yet, oldest first.
    pub fn merge_history(&self, station: &str, mut loaded: Vec<Frame>) {
        let known: Vec<Frame> = self.frames.with_untracked(|frames| {
            frames
                .iter()
                .filter(|f| f.frame.station == station)
                .map(|f| f.frame.clone())
                .collect()
        });
        loaded.retain(|frame| !known.contains(frame));
        if loaded.is_empty() {
            return;
        }
        let mut all: Vec<Frame> = known;
        all.extend(loaded);
        all.sort_by(|a, b| a.ts.cmp(&b.ts));
        let others: Vec<LogFrame> = self.frames.with_untracked(|frames| {
            frames
                .iter()
                .filter(|f| f.frame.station != station)
                .cloned()
                .collect()
        });
        let mut merged: Vec<LogFrame> = others;
        for frame in all {
            let seq = self.seq.get_value();
            self.seq.set_value(seq + 1);
            merged.push(LogFrame { seq, frame });
        }
        merged.sort_by(|a, b| a.frame.ts.cmp(&b.frame.ts).then(a.seq.cmp(&b.seq)));
        let skip = merged.len().saturating_sub(FRAME_CAP);
        self.frames.set(merged.into_iter().skip(skip).collect());
    }

    /// Starts the first load and the event stream; call once.
    pub fn start(self) {
        let first = self;
        Effect::new(move |_| {
            first.tick.track();
            first.need_token.track();
            spawn_local(async move {
                if !first.need_token.get_untracked() {
                    first.refresh_stations().await;
                }
            });
        });
        spawn_local(async move { self.run_stream().await });
    }

    async fn run_stream(self) {
        let last_id: RefCell<Option<String>> = RefCell::new(None);
        let mut delay = 1000;
        loop {
            if self.need_token.get_untracked() {
                util::sleep(300).await;
                continue;
            }
            let Ok(controller) = AbortController::new() else {
                util::sleep(2000).await;
                continue;
            };
            let signal = controller.signal();
            let token = self.token.get_untracked();
            let resume = last_id.borrow().clone();
            let opened = RefCell::new(false);
            let result = api::stream_events(
                token.as_deref(),
                resume.as_deref(),
                &signal,
                |event| {
                    if let Some(id) = &event.id {
                        *last_id.borrow_mut() = Some(id.clone());
                    }
                    self.on_event(&event);
                    true
                },
                || {
                    *opened.borrow_mut() = true;
                    self.live.set(Link::Open);
                    self.bump_soon();
                },
            )
            .await;
            if *opened.borrow() {
                delay = 1000;
            }
            match result {
                Err(ApiError::Unauthorized) => {
                    self.need_token.set(true);
                    self.live.set(Link::Connecting);
                    continue;
                }
                Ok(()) | Err(_) => self.live.set(Link::Reconnecting),
            }
            util::sleep(delay).await;
            delay = (delay * 2).min(15_000);
        }
    }

    fn on_event(&self, event: &api::SseEvent) {
        match event.event.as_str() {
            "frame" => {
                if let Ok(frame) = serde_json::from_str::<Frame>(&event.data) {
                    self.push_frame(frame);
                }
            }
            "connected" | "disconnected" | "state" | "forgotten" | "resync" => {
                // A station that is gone must not stay selected.
                if event.event == "forgotten"
                    && let Ok(StationEvent {
                        station: Some(id), ..
                    }) = serde_json::from_str(&event.data)
                    && self.selected.get_untracked().as_deref() == Some(id.as_str())
                {
                    self.selected.set(None);
                }
                self.bump_soon();
            }
            _ => {}
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StationList {
    stations: Vec<Station>,
}

/// Shorthand for the shared store inside a component.
pub fn use_store() -> Store {
    expect_context::<Store>()
}

/// What `use_fetch` gives back: the latest answer, the latest error text and a loading flag.
#[derive(Debug)]
pub struct Fetched<T: Send + Sync + 'static> {
    pub data: RwSignal<Option<T>>,
    pub error: RwSignal<Option<String>>,
    pub loading: RwSignal<bool>,
    reload: RwSignal<u64>,
}

// Only signals inside: copying never needs `T: Copy`.
impl<T: Send + Sync + 'static> Clone for Fetched<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Send + Sync + 'static> Copy for Fetched<T> {}

impl<T: Send + Sync + 'static> Fetched<T> {
    /// Loads again now.
    pub fn reload(&self) {
        self.reload.update(|n| *n += 1);
    }
}

/// Loads `path()` as JSON and again whenever the path changes, the server reports a change
/// (see [`Store::tick`]) or `reload` is called. An answer to an older request is dropped.
pub fn use_fetch<T>(path: impl Fn() -> String + 'static) -> Fetched<T>
where
    T: for<'de> Deserialize<'de> + Send + Sync + 'static,
{
    let store = use_store();
    let fetched = Fetched {
        data: RwSignal::new(None),
        error: RwSignal::new(None),
        loading: RwSignal::new(false),
        reload: RwSignal::new(0),
    };
    let latest = StoredValue::new(0_u64);
    Effect::new(move |_| {
        store.tick.track();
        fetched.reload.track();
        store.need_token.track();
        let path = path();
        let request = latest.get_value() + 1;
        latest.set_value(request);
        fetched.loading.set(true);
        spawn_local(async move {
            let result = store.get::<T>(&path).await;
            if latest.get_value() != request {
                return;
            }
            fetched.loading.set(false);
            match result {
                Ok(value) => {
                    fetched.data.set(Some(value));
                    fetched.error.set(None);
                }
                Err(error) => fetched.error.set(Some(error.to_string())),
            }
        });
    });
    fetched
}
