//! External Termux adapter — bridges to the user's installed Termux
//! app via the `com.termux.RUN_COMMAND` intent service.
//!
//! Stub. Real implementation lands in its own commit and adds:
//! - JNI calls to construct the `Intent` and call `startService()`
//! - Permission flow: detect Termux installed (PackageManager), prompt
//!   user to grant `com.termux.permission.RUN_COMMAND`, guide them to
//!   set `allow-external-apps=true` in `~/.termux/termux.properties`
//! - Stdio plumbing via abstract socket pairs (intent serializer
//!   doesn't pass fds — we hand it socket paths it then reads/writes)
//! - Result callback handling: register a BroadcastReceiver, parse the
//!   exit-code / stdout / stderr extras

use crate::config::{ExternalTermuxConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

/// Bridges to the user's existing Termux app via Android intents.
/// Slowest of the three adapters (~80ms/spawn from intent serialization
/// + cross-process IPC) but uses the user's already-configured Termux
/// — packages, proot-distro setup, .bashrc, all of it.
pub struct ExternalTermuxAdapter {
    config: ExternalTermuxConfig,
}

impl ExternalTermuxAdapter {
    pub fn new(config: ExternalTermuxConfig) -> anyhow::Result<Self> {
        Ok(Self { config })
    }
}

impl RuntimeProvider for ExternalTermuxAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::ExternalTermux
    }

    fn health_check(&self) -> HealthStatus {
        HealthStatus::NotInstalled {
            hint: format!(
                "ExternalTermuxAdapter health_check stub — external Termux adapter implementation pending. Config: package={}, prefix={}",
                self.config.package,
                self.config.prefix.display(),
            ),
        }
    }

    fn install(&self, _progress: &mut dyn ProgressSink) -> anyhow::Result<()> {
        anyhow::bail!("ExternalTermuxAdapter::install not yet implemented")
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        // No-op by design: Termux is the user's, not ours.
        Ok(())
    }

    fn spawn(&self, _req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        anyhow::bail!("ExternalTermuxAdapter::spawn not yet implemented")
    }
}
