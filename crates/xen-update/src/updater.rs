//! The update flow the window drives: the automatic check (shortly after
//! the start, at most once a day, unless turned off) and the manual one,
//! the offer (Install, Skip This Version, Later), the download with its
//! progress and cancel, the verification, the preparation, and what runs
//! once the window has closed ([`PostExit`]).
//!
//! The work runs on a tokio runtime; the window reads [`Updater::view`]
//! each frame and calls the actions. A failure of the automatic check is
//! only logged; one of the manual check or of an install is shown.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::Product;
use crate::apply::{self, Outcome, Plan, Prepared};
use crate::checksum::{self, ChecksumError};
use crate::client::{Client, ClientError};
use crate::method::{Arch, InstallMethod};
use crate::release::{Asset, Release};
use crate::state::{self, RolledBack, UpdateState};
use crate::version::Version;

/// The download folders' prefix in the temporary folder.
pub const DOWNLOAD_PREFIX: &str = "rexen-center-update";

/// A newer release, and how it would be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub release: Release,
    /// The package for this installation, if the release has one.
    pub package: Option<Asset>,
    /// Whether the release has a checksum file (no checksum, no install).
    pub has_checksums: bool,
}

impl Offer {
    /// The release notes (Markdown as written on the release page).
    pub fn notes(&self) -> &str {
        self.release.body.as_deref().unwrap_or("")
    }

    pub fn page(&self) -> &str {
        &self.release.html_url
    }

    /// Whether this installation can be updated from the window (otherwise
    /// the release page is opened).
    pub fn installable(&self) -> bool {
        self.package.is_some() && self.has_checksums
    }
}

/// How a prepared update is completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyKind {
    /// Restart to finish (files replaced after the window closes).
    Restart,
    /// Quit and run the Windows installer.
    Installer,
    /// Install the Linux package with the system's tools.
    SystemPackage { package: PathBuf, command: String },
}

/// Where the flow is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Checking {
        manual: bool,
    },
    /// The manual check found nothing newer.
    UpToDate,
    Available(Offer),
    Downloading {
        offer: Offer,
        done: u64,
        total: Option<u64>,
    },
    /// Downloaded and verified; preparing it beside the installation.
    Preparing(Offer),
    Ready {
        offer: Offer,
        kind: ReadyKind,
    },
    /// The manual check or the install failed; `offer` is kept so the
    /// release page can still be opened.
    Failed {
        message: String,
        offer: Option<Offer>,
    },
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub phase: Phase,
    /// The update button at the top of the window: an offer the user has
    /// not put off.
    pub button: Option<Version>,
    /// An update rolled back at the last start, to tell once.
    pub rolled_back: Option<RolledBack>,
}

/// What [`run_post_exit`] does once the window has closed.
#[derive(Debug)]
pub enum PostExit {
    /// Replace the installation and start the new version.
    Replace {
        prepared: Prepared,
        scratch: PathBuf,
    },
    /// Run the Windows installer.
    RunInstaller { package: PathBuf },
}

struct Shared {
    phase: Phase,
    state: UpdateState,
    /// The offer was put off ("Later"): no button until the next check.
    dismissed: bool,
    /// The automatic check ran in this session at this time.
    auto_checked: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
    plan: Option<(Plan, PathBuf)>,
    /// Bumped by every new check or download, so a stale task's result is
    /// dropped.
    generation: u64,
}

/// The update flow of one window.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

struct Inner {
    product: Product,
    running: Version,
    method: InstallMethod,
    arch: Arch,
    api: String,
    state_file: Option<PathBuf>,
    runtime: tokio::runtime::Handle,
    started: Instant,
    start_delay: Duration,
    repaint: Box<dyn Fn() + Send + Sync>,
    shared: Mutex<Shared>,
}

/// How to build an [`Updater`].
pub struct Config {
    pub product: Product,
    pub running: Version,
    pub method: InstallMethod,
    pub arch: Arch,
    /// The API address (see [`Client::api_from_env`]).
    pub api: String,
    /// `update.toml`; `None` keeps the state in memory.
    pub state_file: Option<PathBuf>,
    /// The wait before the automatic check ([`state::START_DELAY`]).
    pub start_delay: Duration,
}

impl Updater {
    pub fn new(
        config: Config,
        runtime: tokio::runtime::Handle,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let state = config
            .state_file
            .as_deref()
            .map(UpdateState::load)
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                product: config.product,
                running: config.running,
                method: config.method,
                arch: config.arch,
                api: config.api,
                state_file: config.state_file,
                runtime,
                started: Instant::now(),
                start_delay: config.start_delay,
                repaint: Box::new(repaint),
                shared: Mutex::new(Shared {
                    phase: Phase::Idle,
                    state,
                    dismissed: false,
                    auto_checked: None,
                    cancel: None,
                    plan: None,
                    generation: 0,
                }),
            }),
        }
    }

    pub fn method(&self) -> &InstallMethod {
        &self.inner.method
    }

    pub fn arch(&self) -> Arch {
        self.inner.arch
    }

    pub fn running(&self) -> &Version {
        &self.inner.running
    }

    pub fn view(&self) -> View {
        let shared = self.lock();
        let button = match &shared.phase {
            Phase::Available(offer)
            | Phase::Failed {
                offer: Some(offer), ..
            } if !shared.dismissed => Some(offer.version.clone()),
            Phase::Downloading { offer, .. }
            | Phase::Preparing(offer)
            | Phase::Ready { offer, .. } => Some(offer.version.clone()),
            _ => None,
        };
        View {
            phase: shared.phase.clone(),
            button,
            rolled_back: shared.state.rolled_back.clone(),
        }
    }

    /// Called every frame: starts the automatic check when it is on and
    /// due. Returns how long until it should be called again for that.
    pub fn tick(&self, enabled: bool) -> Option<Duration> {
        if !enabled {
            return None;
        }
        let elapsed = self.inner.started.elapsed();
        if elapsed < self.inner.start_delay {
            return Some(self.inner.start_delay - elapsed);
        }
        let due = {
            let mut shared = self.lock();
            let idle = matches!(shared.phase, Phase::Idle | Phase::UpToDate);
            let again = shared
                .auto_checked
                .is_none_or(|t| t.elapsed() >= state::CHECK_INTERVAL);
            let due = idle && again && shared.state.check_due(true, state::now());
            if due {
                shared.auto_checked = Some(Instant::now());
            }
            due
        };
        if due {
            self.check(false);
        }
        None
    }

    /// Checks for a newer release (`manual`: Check for Updates…).
    pub fn check(&self, manual: bool) {
        let generation = {
            let mut shared = self.lock();
            if matches!(
                shared.phase,
                Phase::Downloading { .. } | Phase::Preparing(_) | Phase::Ready { .. }
            ) {
                // An update is under way; the window shows it.
                return;
            }
            shared.phase = Phase::Checking { manual };
            shared.generation += 1;
            // The throttle counts attempts, so a failing check is not
            // retried every frame.
            shared.state.last_check = Some(state::now());
            self.save(&shared.state);
            shared.generation
        };
        self.repaint();
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let result = this.find_offer(manual).await;
            let mut shared = this.lock();
            if shared.generation != generation {
                return;
            }
            shared.phase = match result {
                Ok(Some(offer)) => {
                    tracing::info!(version = %offer.version, "update available");
                    shared.dismissed = false;
                    Phase::Available(offer)
                }
                Ok(None) if manual => Phase::UpToDate,
                Ok(None) => Phase::Idle,
                Err(e) if manual => Phase::Failed {
                    message: e.to_string(),
                    offer: None,
                },
                Err(e) => {
                    tracing::info!(error = %e, "automatic update check failed");
                    Phase::Idle
                }
            };
            drop(shared);
            this.repaint();
        });
    }

    async fn find_offer(&self, manual: bool) -> Result<Option<Offer>, ClientError> {
        let client = Client::new(&self.inner.product, &self.inner.api)?;
        let release = match client.latest_release().await {
            Ok(r) => r,
            Err(ClientError::NoRelease) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !release.is_stable() {
            return Ok(None);
        }
        let Some(version) = release.version() else {
            return Ok(None);
        };
        let offer = self
            .lock()
            .state
            .should_offer(&version, &self.inner.running, manual);
        if !offer {
            return Ok(None);
        }
        let package = release
            .package_for(&self.inner.product, &self.inner.method, self.inner.arch)
            .cloned();
        let has_checksums = release.checksums().is_some();
        Ok(Some(Offer {
            version,
            release,
            package,
            has_checksums,
        }))
    }

    /// "Skip This Version": the automatic check stops offering it.
    pub fn skip(&self) {
        let mut shared = self.lock();
        let version = match &shared.phase {
            Phase::Available(offer)
            | Phase::Failed {
                offer: Some(offer), ..
            } => Some(offer.version.to_string()),
            _ => None,
        };
        if let Some(version) = version {
            shared.state.skipped_version = Some(version);
            self.save(&shared.state);
        }
        shared.phase = Phase::Idle;
    }

    /// "Later": hides the offer until the next check.
    pub fn later(&self) {
        let mut shared = self.lock();
        shared.dismissed = true;
        if matches!(
            shared.phase,
            Phase::UpToDate | Phase::Failed { offer: None, .. }
        ) {
            shared.phase = Phase::Idle;
        }
    }

    /// Shows a put-off offer again (the button of the dialog).
    pub fn undismiss(&self) {
        self.lock().dismissed = false;
    }

    /// The rollback notice was shown.
    pub fn clear_rolled_back(&self) {
        let mut shared = self.lock();
        if shared.state.rolled_back.take().is_some() {
            self.save(&shared.state);
        }
    }

    /// Downloads, verifies and prepares the offered update.
    pub fn install(&self) {
        let (offer, generation, cancel) = {
            let mut shared = self.lock();
            let offer = match &shared.phase {
                Phase::Available(o) | Phase::Failed { offer: Some(o), .. } => o.clone(),
                _ => return,
            };
            if !offer.installable() {
                return;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            shared.generation += 1;
            shared.cancel = Some(cancel.clone());
            shared.phase = Phase::Downloading {
                offer: offer.clone(),
                done: 0,
                total: offer.package.as_ref().map(|p| p.size).filter(|s| *s > 0),
            };
            (offer, shared.generation, cancel)
        };
        self.repaint();
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let result = this.download_and_prepare(&offer, generation, &cancel).await;
            let mut shared = this.lock();
            if shared.generation != generation {
                // Superseded (cancelled): nothing of it is used.
                if let Ok((plan, scratch)) = result {
                    if let Plan::Replace(prepared) = plan {
                        let _ = std::fs::remove_dir_all(prepared.work);
                    }
                    let _ = std::fs::remove_dir_all(scratch);
                }
                return;
            }
            shared.cancel = None;
            shared.phase = match result {
                Ok((plan, scratch)) => {
                    let kind = match &plan {
                        Plan::Replace(_) => ReadyKind::Restart,
                        Plan::RunInstaller { .. } => ReadyKind::Installer,
                        Plan::SystemPackage { package, command } => ReadyKind::SystemPackage {
                            package: package.clone(),
                            command: command.clone(),
                        },
                    };
                    shared.plan = Some((plan, scratch));
                    Phase::Ready { offer, kind }
                }
                Err(InstallError::Cancelled | InstallError::Client(ClientError::Cancelled)) => {
                    Phase::Available(offer)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "update failed");
                    Phase::Failed {
                        message: e.to_string(),
                        offer: Some(offer),
                    }
                }
            };
            drop(shared);
            this.repaint();
        });
    }

    async fn download_and_prepare(
        &self,
        offer: &Offer,
        generation: u64,
        cancel: &AtomicBool,
    ) -> Result<(Plan, PathBuf), InstallError> {
        let package = offer.package.clone().ok_or(InstallError::NoPackage)?;
        let sums_asset = offer
            .release
            .checksums()
            .cloned()
            .ok_or(InstallError::Checksum(ChecksumError::NoChecksumFile))?;
        let client = Client::new(&self.inner.product, &self.inner.api)?;
        // The checksum first: a release without this file's entry is
        // refused before anything is downloaded.
        let sums = client.checksums(&sums_asset).await?;
        checksum::expected_digest(&sums, &package.name)?;
        let scratch =
            apply::private_dir(DOWNLOAD_PREFIX).map_err(|e| InstallError::Io(e.to_string()))?;
        let result: Result<Plan, InstallError> = async {
            let dest = scratch.join(&package.name);
            let this = self.clone();
            let progress = move |done: u64, total: Option<u64>| {
                let mut shared = this.lock();
                if shared.generation != generation {
                    return;
                }
                if let Phase::Downloading {
                    done: d, total: t, ..
                } = &mut shared.phase
                {
                    *d = done;
                    *t = total;
                }
                drop(shared);
                this.repaint();
            };
            client.download(&package, &dest, &progress, cancel).await?;
            let verify_dest = dest.clone();
            let name = package.name.clone();
            tokio::task::spawn_blocking(move || checksum::verify_file(&verify_dest, &name, &sums))
                .await
                .map_err(|e| InstallError::Io(e.to_string()))??;
            tracing::info!(file = %package.name, "download verified against SHA256SUMS");
            if cancel.load(Ordering::Relaxed) {
                return Err(InstallError::Cancelled);
            }
            {
                let mut shared = self.lock();
                if shared.generation == generation {
                    shared.phase = Phase::Preparing(offer.clone());
                }
            }
            self.repaint();
            let (product, method, version, scratch_dir) = (
                self.inner.product.clone(),
                self.inner.method.clone(),
                offer.version.clone(),
                scratch.clone(),
            );
            let plan = tokio::task::spawn_blocking(move || {
                apply::prepare(&product, &method, &dest, &version, &scratch_dir)
            })
            .await
            .map_err(|e| InstallError::Io(e.to_string()))??;
            Ok(plan)
        }
        .await;
        match result {
            Ok(plan) => Ok((plan, scratch)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&scratch);
                if let Some(work) = apply::work_dir(&self.inner.method) {
                    let _ = std::fs::remove_dir_all(work);
                }
                Err(e)
            }
        }
    }

    /// Cancels the download or drops the prepared update.
    pub fn cancel(&self) {
        let mut shared = self.lock();
        if let Some(cancel) = shared.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let offer = match &shared.phase {
            Phase::Downloading { offer, .. }
            | Phase::Preparing(offer)
            | Phase::Ready { offer, .. } => Some(offer.clone()),
            _ => None,
        };
        if let Some(offer) = offer {
            shared.generation += 1;
            shared.phase = Phase::Available(offer);
        }
        if let Some((plan, scratch)) = shared.plan.take() {
            if let Plan::Replace(prepared) = plan {
                let _ = std::fs::remove_dir_all(prepared.work);
            }
            let _ = std::fs::remove_dir_all(scratch);
        }
    }

    /// Takes the prepared update to complete once the window has closed
    /// (Restart Now, Quit and Install).
    pub fn take_post_exit(&self) -> Option<PostExit> {
        let mut shared = self.lock();
        let (plan, scratch) = shared.plan.take()?;
        match plan {
            Plan::Replace(prepared) => Some(PostExit::Replace { prepared, scratch }),
            Plan::RunInstaller { package } => Some(PostExit::RunInstaller { package }),
            other @ Plan::SystemPackage { .. } => {
                shared.plan = Some((other, scratch));
                None
            }
        }
    }

    /// Writes the remembered state now (the window saves it on exit too).
    pub fn state_file(&self) -> Option<&Path> {
        self.inner.state_file.as_deref()
    }

    fn save(&self, state: &UpdateState) {
        if let Some(file) = &self.inner.state_file
            && let Err(e) = state.save(file)
        {
            tracing::warn!(file = %file.display(), error = %e, "cannot save the update state");
        }
    }

    fn repaint(&self) {
        (self.inner.repaint)();
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why an install stopped.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0}")]
    Client(#[from] ClientError),
    #[error("refused: {0}")]
    Checksum(#[from] ChecksumError),
    #[error("{0}")]
    Apply(#[from] apply::ApplyError),
    #[error("the release has no package for this installation")]
    NoPackage,
    #[error("{0}")]
    Io(String),
    #[error("cancelled")]
    Cancelled,
}

/// Completes an update once the window has closed (and its sessions are
/// logged out): replaces the installation and starts the new version, or
/// runs the installer. A rollback is written to `state_file` for the
/// version started again to report.
pub fn run_post_exit(action: PostExit, state_file: Option<&Path>) {
    match action {
        PostExit::Replace { prepared, scratch } => {
            let version = prepared.version.to_string();
            let relaunch = prepared.relaunch.clone();
            let outcome = match apply::swap(prepared) {
                Ok(swapped) => apply::finish(swapped, &scratch, apply::START_TIMEOUT),
                Err(e) => {
                    // Nothing was replaced; start this version again.
                    if let Err(e) = std::process::Command::new(&relaunch)
                        .env_remove(apply::ACK_ENV)
                        .spawn()
                    {
                        tracing::error!(error = %e, "cannot start again");
                    }
                    Outcome::RolledBack(e.to_string())
                }
            };
            match outcome {
                Outcome::Started => tracing::info!(%version, "updated"),
                Outcome::RolledBack(reason) => {
                    if let Some(file) = state_file {
                        let mut state = UpdateState::load(file);
                        state.rolled_back = Some(RolledBack { version, reason });
                        if let Err(e) = state.save(file) {
                            tracing::error!(error = %e, "cannot record the rollback");
                        }
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&scratch);
        }
        PostExit::RunInstaller { package } => {
            if let Err(e) = apply::run_windows_installer(&package) {
                tracing::error!(error = %e, "cannot run the installer");
            }
        }
    }
}

#[cfg(test)]
mod tests;
