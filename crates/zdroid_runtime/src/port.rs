//! The runtime provider port — the contract every adapter satisfies.
//!
//! `zd-exec` and the settings UI both code against this trait. Adapters
//! plug in; nothing else cares which adapter is active.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::fd::RawFd;
use std::path::PathBuf;

use crate::config::RuntimeId;
use crate::health::{HealthStatus, ProgressSink};

/// A spawn request handed to the active provider. Mirrors the data a
/// `Command::new` carries plus the inheritable fds (passed via
/// `SCM_RIGHTS` in chroot mode, via temp files in external-Termux mode).
///
/// Adapters take this and return a [`SpawnHandle`] representing the
/// running process. The caller waits / signals via the handle.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Target binary name as the user / Zed wants it (e.g. `git`,
    /// `rust-analyzer`). The adapter resolves this against its userland
    /// (e.g. `/usr/bin/git` inside a chroot, `$PREFIX/bin/git` in
    /// bootstrap mode, `bash -c git ...` via Termux RUN_COMMAND).
    pub program: String,

    /// argv[1..] for the child. argv[0] is set by the adapter to match
    /// `program` (or its absolute path, depending on lookup semantics).
    pub args: Vec<OsString>,

    /// Working directory the child should land in. Adapters translate
    /// host-side paths to their userland's view (e.g.
    /// `/data/.../home/projects/foo` → `/zed/projects/foo` inside a
    /// chroot whose `/zed` is bind-mounted to the host's home).
    pub cwd: Option<PathBuf>,

    /// Env vars to set for the child. The adapter merges with whatever
    /// baseline its userland needs (PATH inside the rootfs, HOME,
    /// TERM, etc.). Anything in this map wins.
    pub env: HashMap<String, OsString>,

    /// True if this spawn is replacing an interactive terminal. Adapters
    /// use this to decide whether to allocate a controlling tty,
    /// pass-through `setsid -c`, source a login shell, etc. Non-tty
    /// spawns (LSPs, git, formatters) skip those costs.
    pub interactive: bool,

    /// Stdio fds to pass to the child. Indexed 0=stdin, 1=stdout,
    /// 2=stderr. Typically `[0, 1, 2]` for "inherit the caller's
    /// stdio". Adapters that bridge across processes (chroot via
    /// daemon, external Termux via intent) duplicate these fds across
    /// the boundary using `SCM_RIGHTS` or equivalent. The fds remain
    /// valid for the caller; ownership is not transferred.
    pub stdio: [RawFd; 3],
}

/// A running child process spawned through an adapter. Exact mechanism
/// is adapter-specific (subprocess pid, daemon-tracked job, intent
/// callback handle), but the surface is uniform.
pub trait SpawnHandle: Send {
    /// Block until the child exits, return its exit code. -1 if killed
    /// by a signal that doesn't map to an exit code (the adapter logs
    /// the signal number).
    fn wait(&mut self) -> anyhow::Result<i32>;

    /// Send SIGKILL (or adapter-equivalent termination signal).
    fn kill(&mut self) -> anyhow::Result<()>;
}

/// The port. Implementations are in `adapters::{chroot, bootstrap,
/// external_termux}`.
pub trait RuntimeProvider: Send + Sync {
    /// Stable identifier for this provider — picked into `runtime.toml`
    /// and shown in the settings UI.
    fn id(&self) -> RuntimeId;

    /// Quick feasibility check ("ping"). Should complete in <1s for
    /// healthy state, <3s for failure modes. Settings UI calls this on
    /// page render to show a green/yellow/red dot per adapter.
    fn health_check(&self) -> HealthStatus;

    /// One-shot install / setup. Idempotent: calling on an
    /// already-installed adapter is a no-op + Ok. Long operations
    /// (downloading a bootstrap tarball, walking a rootfs) report
    /// progress through the sink.
    fn install(&self, progress: &mut dyn ProgressSink) -> anyhow::Result<()>;

    /// Remove anything this adapter installed. Adapter-specific:
    /// bootstrap removes its `$PREFIX/{bin,lib,etc}`; chroot is no-op
    /// (the rootfs is the user's, not ours); external Termux is no-op.
    /// Project files / settings / config are never touched.
    fn uninstall(&self) -> anyhow::Result<()>;

    /// Spawn the child process described by `req`. See [`SpawnHandle`]
    /// for the returned handle's contract.
    fn spawn(&self, req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>>;

    /// True if switching TO this adapter from another requires the app
    /// to restart. Most adapters return true because env init (PATH,
    /// HOME, etc.) runs once at boot and won't pick up the new shape
    /// otherwise. Settings UI uses this to gate the "Restart Zdroid?"
    /// prompt during a switch.
    fn requires_restart_on_switch(&self) -> bool {
        true
    }
}
