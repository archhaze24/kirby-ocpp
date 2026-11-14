//! A small timer wheel for actor loops.
//!
//! An actor needs a handful of named one-shot timers (the station's heartbeat, reconnect
//! backoff, boot retry, `MinimumStatusDuration` debounce per connector, persistence flush, ...).
//! [`TimerWheel`] owns them:
//!
//! * **Keyed and re-armable:** [`TimerWheel::set`] on an armed key replaces the old
//!   timer; the replaced timer can never fire (each arming carries a generation, stale
//!   fires are filtered).
//! * **Cancellable:** [`TimerWheel::cancel`] and friends; a cancelled timer never fires.
//! * **Delivered to the actor loop:** the wheel is a single event source:
//!   `select!` on [`TimerWheel::next`], no callbacks, no locks. `next` is cancel-safe.
//! * **Any length:** a delay is slept in steps of at most [`MAX_DELAY`] (so `Instant`
//!   arithmetic cannot overflow) until the whole delay has elapsed; only an absurd delay
//!   beyond [`MAX_TOTAL_DELAY`] is capped there.
//!
//! Timers use `tokio::time`, never the injected wall [`Clock`](crate::clock::Clock).
//! A zero delay fires on the next poll; callers of *periodic* timers must never pass a
//! zero period (the heartbeat interval is clamped to >= 1 s before it gets here).
//!
//! The wheel knows nothing about OCPP; it is also reachable as `station::timers`.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Longest single sleep of a timer task (24 hours); longer delays are slept in several steps.
pub const MAX_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Longest total delay of one arming (200 years; a `u32` number of seconds is below it).
pub const MAX_TOTAL_DELAY: Duration = Duration::from_secs(200 * 365 * 24 * 60 * 60);

struct Armed {
    generation: u64,
    task: JoinHandle<()>,
}

impl Drop for Armed {
    fn drop(&mut self) {
        // A no-op for a timer that already fired.
        self.task.abort();
    }
}

/// Named one-shot timers feeding one receiver.
pub struct TimerWheel<K> {
    tx: mpsc::UnboundedSender<(K, u64)>,
    rx: mpsc::UnboundedReceiver<(K, u64)>,
    armed: HashMap<K, Armed>,
    next_generation: u64,
}

impl<K> std::fmt::Debug for TimerWheel<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimerWheel")
            .field("armed", &self.armed.len())
            .finish_non_exhaustive()
    }
}

impl<K> Default for TimerWheel<K>
where
    K: Clone + Eq + Hash + Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K> TimerWheel<K>
where
    K: Clone + Eq + Hash + Send + 'static,
{
    /// An empty wheel. Must be used inside a tokio runtime (timers are tasks).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx,
            armed: HashMap::new(),
            next_generation: 0,
        }
    }

    /// Arms `key` to fire after `delay` (slept in steps of at most [`MAX_DELAY`], so a delay
    /// of any length is honoured); an armed `key` is replaced.
    pub fn set(&mut self, key: K, delay: Duration) {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        let tx = self.tx.clone();
        let fired = key.clone();
        let task = tokio::spawn(async move {
            let mut remaining = delay.min(MAX_TOTAL_DELAY);
            while remaining > MAX_DELAY {
                sleep(MAX_DELAY).await;
                remaining -= MAX_DELAY;
            }
            sleep(remaining).await;
            // The wheel may be gone by now.
            let _ = tx.send((fired, generation));
        });
        // Dropping the previous entry aborts its task.
        self.armed.insert(key, Armed { generation, task });
    }

    /// Arms `key` only if it is not armed already. Returns `true` if it was armed now.
    pub fn set_if_idle(&mut self, key: K, delay: Duration) -> bool {
        if self.armed.contains_key(&key) {
            return false;
        }
        self.set(key, delay);
        true
    }

    /// Disarms `key`. Returns whether it was armed.
    pub fn cancel(&mut self, key: &K) -> bool {
        self.armed.remove(key).is_some()
    }

    /// Disarms every key for which `predicate` holds.
    pub fn cancel_where(&mut self, mut predicate: impl FnMut(&K) -> bool) {
        self.armed.retain(|key, _| !predicate(key));
    }

    /// Disarms everything.
    pub fn cancel_all(&mut self) {
        self.armed.clear();
    }

    /// Whether `key` is armed and has not fired yet.
    pub fn is_armed(&self, key: &K) -> bool {
        self.armed.contains_key(key)
    }

    /// Number of armed timers.
    pub fn len(&self) -> usize {
        self.armed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.armed.is_empty()
    }

    /// Waits for the next timer to fire and returns its key (which is then disarmed).
    /// Cancel-safe. Pending forever while nothing is armed.
    pub async fn next(&mut self) -> K {
        loop {
            // The wheel owns a sender, so the channel never closes.
            let Some((key, generation)) = self.rx.recv().await else {
                return std::future::pending().await;
            };
            let current = self
                .armed
                .get(&key)
                .is_some_and(|armed| armed.generation == generation);
            if current {
                self.armed.remove(&key);
                return key;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::{advance, timeout};

    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// Runs the paused clock forward in small steps, letting spawned timer tasks run.
    async fn run_for(duration: Duration) {
        advance(duration).await;
        // Let the woken timer tasks and the channel deliver.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn fires_after_the_delay() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(100));
        assert!(wheel.is_armed(&"a"));
        assert!(timeout(MS(99), wheel.next()).await.is_err());
        assert_eq!(timeout(MS(10), wheel.next()).await, Ok("a"));
        assert!(!wheel.is_armed(&"a"), "a fired timer is disarmed");
        assert!(wheel.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn rearming_replaces_the_old_timer() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(100));
        wheel.set("a", MS(300));
        assert_eq!(wheel.len(), 1);
        assert!(timeout(MS(200), wheel.next()).await.is_err());
        assert_eq!(timeout(MS(150), wheel.next()).await, Ok("a"));
        // The replaced timer must not fire a second time.
        assert!(timeout(MS(1_000), wheel.next()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn rearming_to_a_shorter_delay_fires_once_and_early() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(500));
        wheel.set("a", MS(50));
        assert_eq!(timeout(MS(60), wheel.next()).await, Ok("a"));
        assert!(timeout(MS(1_000), wheel.next()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_timers_never_fire() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(10));
        wheel.set("b", MS(20));
        assert!(wheel.cancel(&"a"));
        assert!(!wheel.cancel(&"a"));
        assert_eq!(timeout(MS(100), wheel.next()).await, Ok("b"));
        assert!(timeout(MS(100), wheel.next()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_fire_already_queued_is_dropped_when_cancelled_after_the_fact() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(10));
        run_for(MS(20)).await; // fired into the channel, not yet received
        assert!(wheel.cancel(&"a"));
        assert!(timeout(MS(100), wheel.next()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn keys_fire_in_deadline_order() {
        let mut wheel = TimerWheel::new();
        wheel.set(3, MS(30));
        wheel.set(1, MS(10));
        wheel.set(2, MS(20));
        let mut order = Vec::new();
        for _ in 0..3 {
            order.push(wheel.next().await);
        }
        assert_eq!(order, [1, 2, 3]);
    }

    #[tokio::test(start_paused = true)]
    async fn set_if_idle_does_not_replace() {
        let mut wheel = TimerWheel::new();
        assert!(wheel.set_if_idle("a", MS(100)));
        assert!(!wheel.set_if_idle("a", MS(10)));
        assert!(timeout(MS(50), wheel.next()).await.is_err());
        assert_eq!(timeout(MS(100), wheel.next()).await, Ok("a"));
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_where_and_cancel_all() {
        let mut wheel = TimerWheel::new();
        wheel.set(1, MS(10));
        wheel.set(2, MS(10));
        wheel.set(3, MS(10));
        wheel.cancel_where(|key| *key != 2);
        assert_eq!(wheel.len(), 1);
        assert_eq!(wheel.next().await, 2);
        wheel.set(4, MS(10));
        wheel.cancel_all();
        assert!(wheel.is_empty());
        assert!(timeout(MS(100), wheel.next()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn absurd_delays_are_capped_not_panicking() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", Duration::MAX);
        assert!(wheel.is_armed(&"a"));
        // Fires after the cap, i.e. after at most 200 years of (virtual) time.
        assert_eq!(wheel.next().await, "a");
    }

    #[tokio::test(start_paused = true)]
    async fn delays_longer_than_a_day_are_honoured() {
        let mut wheel = TimerWheel::new();
        let two_days = MAX_DELAY * 2 + Duration::from_secs(5);
        wheel.set("a", two_days);
        // Not after the old 24 h clamp, nor shortly before the real deadline ...
        assert!(timeout(MAX_DELAY * 2, wheel.next()).await.is_err());
        // ... but at it.
        assert_eq!(
            timeout(Duration::from_secs(10), wheel.next()).await,
            Ok("a")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_delay_fires_immediately() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", Duration::ZERO);
        assert_eq!(timeout(MS(1), wheel.next()).await, Ok("a"));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_wheel_stops_its_timers() {
        let mut wheel = TimerWheel::new();
        wheel.set("a", MS(10));
        drop(wheel);
        // Nothing to assert beyond "no panic / no leaked task keeps the runtime busy".
        advance(MS(100)).await;
    }
}
