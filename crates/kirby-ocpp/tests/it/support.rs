//! Shared helpers for tests that run the real binary: every wait has a deadline and a child
//! that outlives it is killed, so a hung process fails one test instead of hanging the run.

use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Longest a child may run before it is killed.
pub const CHILD_LIMIT: Duration = Duration::from_secs(60);

/// A running child that is killed and reaped when dropped (a failed assertion included).
pub struct ChildGuard(Option<Child>);

impl ChildGuard {
    pub fn spawn(command: &mut Command) -> Self {
        Self(Some(command.spawn().expect("binary runs")))
    }

    #[cfg(unix)]
    pub fn id(&self) -> u32 {
        self.0.as_ref().expect("child is running").id()
    }

    /// Waits for the exit and collects stdout and stderr (a pipe that was taken or not piped
    /// stays empty). Kills the child and panics when `limit` passes first.
    pub fn finish(mut self, limit: Duration) -> Output {
        let mut child = self.0.take().expect("child is running");
        let readers = [child.stdout.take().map(pump), child.stderr.take().map(pump)];
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break status;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child still running after {limit:?}; killed");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let [stdout, stderr] = readers.map(|reader| {
            reader
                .map(|reader| reader.join().unwrap_or_default())
                .unwrap_or_default()
        });
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn pump<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// `Command::output` with a deadline: kills the child after [`CHILD_LIMIT`].
pub fn output(mut command: Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    ChildGuard::spawn(&mut command).finish(CHILD_LIMIT)
}

/// Async wrapper around [`ChildGuard::finish`] for tests on the multi-thread runtime.
pub async fn finish(child: ChildGuard) -> Output {
    tokio::task::spawn_blocking(move || child.finish(CHILD_LIMIT))
        .await
        .expect("finish task")
}
