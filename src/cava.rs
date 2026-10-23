//! Live audio bands from `cava`, run on a background thread with the same settings as the
//! Quickshell media widget: wave mode, 50 mono bars at 60 fps as raw ASCII values.

use std::io::{BufRead, BufReader, ErrorKind};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log::{debug, info, warn};

const CONFIG: &str = "\
[general]
mode = waves
framerate = 60
autosens = 1
bars = 50

[output]
method = raw
raw_target = /dev/stdout
data_format = ascii
channels = mono
mono_option = average

[smoothing]
noise_reduction = 20
";

/// Pause between checks of whether cava should run.
const POLL: Duration = Duration::from_millis(100);
/// Wait before restarting cava after it exits on its own, doubling up to the maximum.
const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(30);

/// Parses one line of cava's raw ASCII output, `;`-separated values.
pub fn parse_line(line: &str) -> Vec<f32> {
    line.split(';')
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .collect()
}

#[derive(Default)]
struct Shared {
    wanted: AtomicBool,
    quit: AtomicBool,
    bands: Mutex<Vec<f32>>,
    child: Mutex<Option<Child>>,
}

impl Shared {
    fn bands(&self) -> MutexGuard<'_, Vec<f32>> {
        self.bands.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn child(&self) -> MutexGuard<'_, Option<Child>> {
        self.child.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn kill(&self) {
        if let Some(child) = self.child().as_mut() {
            let _ = child.kill();
        }
    }
}

/// Runs cava while active. Without cava installed it stays silent and yields no bands.
pub struct Cava {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Cava {
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("cava".into())
            .spawn(move || supervise(&worker))
            .map_err(|e| warn!("failed to start cava thread: {e}"))
            .ok();
        Self { shared, handle }
    }

    /// Starts or stops cava.
    pub fn set_active(&self, active: bool) {
        if self.shared.wanted.swap(active, Ordering::Relaxed) && !active {
            self.shared.kill();
        }
    }

    /// The latest bands, empty while cava is not running.
    pub fn bands(&self) -> Vec<f32> {
        self.shared.bands().clone()
    }
}

impl Default for Cava {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Cava {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        self.shared.wanted.store(false, Ordering::Relaxed);
        self.shared.kill();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("mpd-display-rs");
    std::fs::create_dir_all(&dir)
        .map_err(|e| warn!("failed to create {}: {e}", dir.display()))
        .ok()?;
    let path = dir.join("cava.conf");
    std::fs::write(&path, CONFIG)
        .map_err(|e| warn!("failed to write {}: {e}", path.display()))
        .ok()?;
    Some(path)
}

/// Keeps cava running while it is wanted, restarting it with a growing delay if it exits.
fn supervise(shared: &Shared) {
    let mut config = None;
    let mut backoff = RESTART_MIN;
    while !shared.quit.load(Ordering::Relaxed) {
        if !shared.wanted.load(Ordering::Relaxed) {
            backoff = RESTART_MIN;
            thread::sleep(POLL);
            continue;
        }
        if config.is_none() {
            config = config_path();
            if config.is_none() {
                return;
            }
        }
        let Some(path) = config.as_ref() else { return };
        let spawned = Command::new("cava")
            .arg("-p")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                info!("cava not found; the visualizer is off");
                return;
            }
            Err(e) => {
                warn!("failed to start cava: {e}");
                return;
            }
        };
        let stdout = child.stdout.take();
        *shared.child() = Some(child);
        if !shared.wanted.load(Ordering::Relaxed) {
            shared.kill();
        }
        let mut got_output = false;
        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let bands = parse_line(&line);
                if !bands.is_empty() {
                    got_output = true;
                    *shared.bands() = bands;
                }
            }
        }
        if let Some(mut child) = shared.child().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        shared.bands().clear();
        if shared.wanted.load(Ordering::Relaxed) && !shared.quit.load(Ordering::Relaxed) {
            // Exited on its own: try again later.
            if got_output {
                backoff = RESTART_MIN;
            }
            if backoff == RESTART_MIN {
                warn!("cava exited; restarting it");
            } else {
                debug!("cava exited again; retrying in {}s", backoff.as_secs());
            }
            let mut waited = Duration::ZERO;
            while waited < backoff && shared.wanted.load(Ordering::Relaxed) {
                thread::sleep(POLL);
                waited += POLL;
            }
            backoff = (backoff * 2).min(RESTART_MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_ascii_lines() {
        assert_eq!(parse_line("0;12;1000;"), [0.0, 12.0, 1000.0]);
        assert_eq!(parse_line(" 3 ; x ;4"), [3.0, 4.0]);
        assert!(parse_line("").is_empty());
    }

    #[test]
    fn config_matches_widget_settings() {
        for setting in [
            "mode = waves",
            "framerate = 60",
            "bars = 50",
            "data_format = ascii",
            "channels = mono",
            "noise_reduction = 20",
        ] {
            assert!(CONFIG.contains(setting), "{setting}");
        }
    }
}
