//! Chroot adapter — talks to `zd-spawnd` over a Unix socket.
//!
//! Stub. Real implementation lands in its own commit and adds:
//! - `zd-spawnd` connection management (lazy connect, retry on EPIPE)
//! - `SCM_RIGHTS` fd passing for stdio
//! - request/response framing
//! - fallback to `RUNTIME_SU` if the daemon socket is unreachable
//!   (typical during first install before the Magisk module ran)
//!
//! The wire protocol with `zd-spawnd` is documented alongside the
//! daemon source at `crates/gpui_android/native/zd-spawnd/PROTOCOL.md`.

use crate::config::{ChrootConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

/// Connects to `zd-spawnd` per spawn. The daemon holds the elevated
/// chroot context; we pay socket roundtrip + fork cost (~5ms total)
/// instead of Magisk `su` mediation per call (~200ms + queue).
pub struct ChrootAdapter {
    config: ChrootConfig,
}

impl ChrootAdapter {
    pub fn new(config: ChrootConfig) -> anyhow::Result<Self> {
        Ok(Self { config })
    }
}

impl RuntimeProvider for ChrootAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::Chroot
    }

    fn health_check(&self) -> HealthStatus {
        HealthStatus::NotInstalled {
            hint: format!(
                "ChrootAdapter health_check stub — chroot adapter implementation pending. Config: root={}, socket={}",
                self.config.root.display(),
                self.config.spawnd_socket.display(),
            ),
        }
    }

    fn install(&self, _progress: &mut dyn ProgressSink) -> anyhow::Result<()> {
        anyhow::bail!("ChrootAdapter::install not yet implemented")
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        // No-op by design: the chroot rootfs is the user's, not ours.
        // Magisk module removal is handled by Magisk Manager, not us.
        Ok(())
    }

    fn spawn(&self, _req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        anyhow::bail!("ChrootAdapter::spawn not yet implemented")
    }
}
