//! Every change to a Steam library file (`appmanifest_<appid>.acf`,
//! `libraryfolders.vdf`) goes through a [`Lease`]. A running Steam reads these files
//! at startup and writes its in-memory copy back on the app's next state change, so
//! a change made under it is undone. A lease exists only after [`Hold::lease`]'s
//! check passed, and it lives as long as the work that writes.
use super::*;
use crate::core::error::{ErrorKind, TypedError};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The per-command choice a [`SteamClient`] carries, and what its leases did.
#[derive(Clone, Default)]
pub(crate) struct SteamFileAccess {
    may_restart: bool,
    report: Arc<Mutex<AccessReport>>,
}

#[derive(Default)]
struct AccessReport {
    stopped_steam: bool,
    restart_error: Option<anyhow::Error>,
}

/// How Aurelia controls host Steam. Plain fn pointers so a test swaps in a fake.
struct SteamControl {
    running: fn() -> bool,
    stop: fn() -> Result<()>,
    start: fn() -> Result<()>,
}

static HOST_STEAM: SteamControl = SteamControl {
    running: SteamClient::steam_is_running,
    stop: SteamClient::shutdown_steam,
    start: SteamClient::start_steam,
};

/// Every lease in this process. All commands run in the one daemon.
static HOST_HOLD: Hold = Hold::new(&HOST_STEAM);

enum Admit {
    Proceed,
    StopSteam,
    Refuse,
}

/// Steam holds a file it read at startup, approximated as "Steam runs and the file
/// exists". A file that doesn't exist yet can't be held.
fn decide(steam_running: bool, touches_existing: bool, may_restart: bool) -> Admit {
    match (steam_running && touches_existing, may_restart) {
        (false, _) => Admit::Proceed,
        (true, true) => Admit::StopSteam,
        (true, false) => Admit::Refuse,
    }
}

struct HoldState {
    /// Live leases of every kind, fresh-file ones included.
    leases: usize,
    /// A lease stopped Steam and nothing has started it since.
    stopped: bool,
}

struct Hold {
    state: Mutex<HoldState>,
    /// Two leases on one file must not lose each other's read-modify-write.
    writes: Mutex<()>,
    steam: &'static SteamControl,
}

impl Hold {
    const fn new(steam: &'static SteamControl) -> Self {
        Self {
            state: Mutex::new(HoldState {
                leases: 0,
                stopped: false,
            }),
            writes: Mutex::new(()),
            steam,
        }
    }

    /// Blocking: a stop waits up to 30 s. It holds the state lock across the stop
    /// so a concurrent lease never sees Steam half down.
    fn lease(&'static self, files: Vec<PathBuf>, access: SteamFileAccess) -> Result<Lease> {
        let mut state = lock(&self.state);
        let running = (self.steam.running)();
        let held = files.iter().find(|f| f.exists());
        let named = || {
            held.or(files.first())
                .map_or_else(String::new, |f| f.display().to_string())
        };
        let stopped_steam = match decide(running, held.is_some(), access.may_restart) {
            Admit::Proceed => false,
            Admit::Refuse => return Err(steam_running_error(&named())),
            Admit::StopSteam => {
                tracing::warn!(
                    "Stopping Steam, which would undo this change to {}. It starts again when \
                     Aurelia is done.",
                    named()
                );
                (self.steam.stop)()?;
                state.stopped = true;
                lock(&access.report).stopped_steam = true;
                true
            }
        };
        state.leases += 1;
        Ok(Lease {
            hold: self,
            access,
            files,
            steam_down: !running || stopped_steam,
            stopped_steam,
        })
    }

    /// Start Steam under the state lock, so a lease admitted next sees it running.
    fn release(&self, stopped_steam: bool, access: &SteamFileAccess) {
        let mut state = lock(&self.state);
        state.leases -= 1;
        if state.leases > 0 {
            if stopped_steam {
                tracing::warn!("Steam stays closed until Aurelia's other Steam-file changes end");
            }
            return;
        }
        if !std::mem::take(&mut state.stopped) {
            return;
        }
        tracing::info!("Starting Steam again");
        if let Err(err) = (self.steam.start)() {
            tracing::error!("failed starting Steam again: {err:#}");
            lock(&access.report).restart_error = Some(err);
        }
    }
}

/// Permission to change `files`. Move it into the work that writes. However it
/// ends, it releases, and the last release starts a Steam that a lease stopped.
#[must_use = "dropping a lease ends it"]
pub(crate) struct Lease {
    hold: &'static Hold,
    access: SteamFileAccess,
    files: Vec<PathBuf>,
    /// Steam was not running once the lease was granted.
    steam_down: bool,
    stopped_steam: bool,
}

impl Lease {
    /// Read-modify-write one leased file. `edit` gets its text (`None` if absent)
    /// and returns the new text, or `None` to leave it. Returns whether it wrote.
    pub(crate) fn rewrite(
        &self,
        file: &Path,
        edit: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    ) -> Result<bool> {
        self.check_covers(file)?;
        let _writing = lock(&self.hold.writes);
        let old = match std::fs::read_to_string(file) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("failed reading {}", file.display())),
        };
        let new =
            edit(old.as_deref()).with_context(|| format!("failed updating {}", file.display()))?;
        let Some(new) = new.filter(|new| old.as_ref() != Some(new)) else {
            return Ok(false);
        };
        self.warn_if_steam_started(file);
        crate::core::atomic::write_atomic(file, new.as_bytes())?;
        Ok(true)
    }

    /// Move one leased file to another leased path.
    pub(crate) fn move_file(&self, from: &Path, to: &Path) -> Result<()> {
        self.check_covers(from)?;
        self.check_covers(to)?;
        let _writing = lock(&self.hold.writes);
        self.warn_if_steam_started(from);
        std::fs::copy(from, to).with_context(|| format!("failed writing {}", to.display()))?;
        std::fs::remove_file(from).with_context(|| format!("failed removing {}", from.display()))
    }

    /// End the lease before reporting completion, so the restart it may trigger,
    /// and that restart's error in [`SteamClient::steam_files_report`], come first.
    pub(crate) fn finish(self) {}

    fn check_covers(&self, file: &Path) -> Result<()> {
        if !self.files.iter().any(|f| f == file) {
            bail!("{} is not covered by this Steam-file lease", file.display());
        }
        Ok(())
    }

    fn warn_if_steam_started(&self, file: &Path) {
        if self.steam_down && (self.hold.steam.running)() {
            tracing::warn!(
                "Steam started while Aurelia was changing {}, so Steam may undo the change",
                file.display()
            );
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.hold.release(self.stopped_steam, &self.access);
    }
}

fn steam_running_error(file: &str) -> anyhow::Error {
    TypedError::new(
        ErrorKind::SteamRunning,
        format!(
            "Steam is running and would undo this change to {file}. It writes its own copy \
             back on the game's next launch or exit. Close Steam first, or re-run with \
             --restart-steam to have Aurelia stop and restart it."
        ),
    )
    .into()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Some Steam-file change is running, whether or not it stopped Steam.
pub(crate) fn steam_files_leased() -> bool {
    lock(&HOST_HOLD.state).leases > 0
}

/// Start a Steam that a lease stopped, for a process about to exit: the spawned
/// work holding the lease dies with the process and never releases it.
pub fn restart_stopped_steam() {
    if !std::mem::take(&mut lock(&HOST_HOLD.state).stopped) {
        return;
    }
    if let Err(err) = (HOST_HOLD.steam.start)() {
        tracing::error!("failed starting Steam again: {err:#}");
    }
}

impl SteamClient {
    /// Let this client's Steam-file changes stop a running Steam and start it again
    /// afterward. Without it they fail with [`ErrorKind::SteamRunning`] whenever
    /// Steam holds a file they would change. Files that don't exist yet never need it.
    pub fn allow_steam_restart(mut self, allow: bool) -> Self {
        self.steam_files.may_restart = allow;
        self
    }

    /// Whether a Steam-file change made through this client stopped Steam, or the
    /// error from starting it again.
    pub fn steam_files_report(&self) -> Result<bool> {
        let mut report = lock(&self.steam_files.report);
        match report.restart_error.take() {
            Some(err) => Err(err),
            None => Ok(report.stopped_steam),
        }
    }

    /// The check every Steam-file writer runs before any long work.
    pub(crate) async fn lease_steam_files(&self, files: &[&Path]) -> Result<Lease> {
        let files = files.iter().map(|f| f.to_path_buf()).collect();
        let access = self.steam_files.clone();
        tokio::task::spawn_blocking(move || HOST_HOLD.lease(files, access)).await?
    }
}

#[cfg(test)]
mod tests;
