//! Bootstrap adapter — owns a Termux-flavored `$PREFIX` inside Zdroid's
//! sandbox. Tarball is downloaded from a separate GitHub repo
//! (`Dylanmurzello/zdroid-bootstrap`) at `install()` time; nothing is
//! bundled with the editor APK.
//!
//! Stub. Real implementation lands in its own commit and adds:
//! - GitHub releases API client (latest release, asset URL, sha256)
//! - Streaming download with progress reporting
//! - tar.zst extraction into a staging dir, atomic rename to live
//! - Optional `proot -r <rootfs>` wrapping for tools that need glibc
//! - Direct `$PREFIX/bin/<target>` exec for bare-mode tools

use crate::config::{BootstrapConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

/// Bootstrap adapter. Two operating modes inside our sandbox:
/// 1. Bare: spawn directly via `$PREFIX/bin/<target>` (bionic).
/// 2. Proot: wrap each spawn in `proot -r <proot_rootfs> -- <target>`,
///    landing inside a glibc rootfs that lives at
///    `$PREFIX/var/proot/<distro>`.
pub struct BootstrapAdapter {
    config: BootstrapConfig,
}

impl BootstrapAdapter {
    pub fn new(config: BootstrapConfig) -> anyhow::Result<Self> {
        Ok(Self { config })
    }
}

impl RuntimeProvider for BootstrapAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::Bootstrap
    }

    fn health_check(&self) -> HealthStatus {
        HealthStatus::NotInstalled {
            hint: format!(
                "BootstrapAdapter health_check stub — bootstrap adapter implementation pending. Config: prefix={}, proot_rootfs={:?}, repo={}",
                self.config.prefix.display(),
                self.config.proot_rootfs,
                self.config.release_repo,
            ),
        }
    }

    fn install(&self, _progress: &mut dyn ProgressSink) -> anyhow::Result<()> {
        anyhow::bail!("BootstrapAdapter::install not yet implemented")
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        anyhow::bail!("BootstrapAdapter::uninstall not yet implemented")
    }

    fn spawn(&self, _req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        anyhow::bail!("BootstrapAdapter::spawn not yet implemented")
    }
}
