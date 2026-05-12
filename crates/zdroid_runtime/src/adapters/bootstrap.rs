//! Bootstrap adapter — owns a Termux-flavored `$PREFIX` inside Zdroid's
//! own app sandbox.
//!
//! Two operating modes inside the sandbox:
//!
//! 1. **Bare** (default): spawn directly via `$PREFIX/bin/<target>`.
//!    Bionic-linked binaries from the bootstrap. Zero per-spawn cost
//!    over a normal `Command::new`.
//! 2. **Proot**: wrap each spawn in `proot -r <proot_rootfs> -- ...`,
//!    landing inside a glibc rootfs that lives at
//!    `$PREFIX/var/proot/<distro>`. ~10-15ms ptrace overhead per
//!    syscall but full glibc tools.
//!
//! The bootstrap itself (the rebuilt-Termux userland tarball) lives in
//! a separate GitHub repo and is downloaded on `install()`. Editor
//! releases ship without it; the editor's APK doesn't bundle a
//! userland anymore.

use std::path::PathBuf;

use crate::config::{BootstrapConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

/// Manages our termux-flavored userland inside the Zdroid sandbox.
pub struct BootstrapAdapter {
    config: BootstrapConfig,
}

impl BootstrapAdapter {
    pub fn new(config: BootstrapConfig) -> anyhow::Result<Self> {
        Ok(Self { config })
    }

    /// Resolve the absolute path of a target binary inside the
    /// bootstrap. We look in `$PREFIX/bin` first, then `$PREFIX/sbin`.
    /// Returns None if neither has the target.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn resolve_target(&self, program: &str) -> Option<PathBuf> {
        let candidates = [
            self.config.prefix.join("bin").join(program),
            self.config.prefix.join("sbin").join(program),
        ];
        candidates.into_iter().find(|p| p.exists())
    }
}

#[cfg(target_os = "android")]
mod android_impl {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::fd::BorrowedFd;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use anyhow::{Context, Result};

    use crate::config::BootstrapConfig;
    use crate::port::{SpawnHandle, SpawnRequest};

    /// Wraps a `std::process::Child` so the rest of zdroid_runtime can
    /// treat bootstrap-spawned processes like daemon-spawned ones via
    /// the [`SpawnHandle`] trait. The handle holds the child by value;
    /// dropping kills+reaps if not already exited.
    pub(super) struct BootstrapSpawnHandle {
        child: std::process::Child,
    }

    impl SpawnHandle for BootstrapSpawnHandle {
        fn wait(&mut self) -> Result<i32> {
            let status = self.child.wait().context("wait for bootstrap child")?;
            // Termination by signal: surface as -signum to match the
            // chroot adapter's convention.
            if let Some(code) = status.code() {
                Ok(code)
            } else {
                Ok(-1)
            }
        }

        fn kill(&mut self) -> Result<()> {
            self.child.kill().context("kill bootstrap child")
        }
    }

    /// Build a `Command` for the request, applying cwd/env/stdio. Used
    /// by both bare and proot dispatch (the proot path just exec's
    /// proot with the original target as a tail arg).
    fn build_base_command(
        program: &Path,
        args: &[OsString],
        cwd: Option<&Path>,
        env: &HashMap<String, OsString>,
        stdio: [std::os::fd::RawFd; 3],
    ) -> Result<Command> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        // Replace env entirely with what the caller asked for, plus
        // baseline values the bootstrap expects (PATH, HOME, TERM)
        // unless the caller set them.
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }

        // Dup the caller's stdio fds into owned descriptors so
        // Command can take them. Without dup the original fds would be
        // moved into the child and closed when our request goes out of
        // scope, breaking the caller's view of stdin/stdout/stderr.
        for (i, fd) in stdio.iter().enumerate() {
            // SAFETY: caller asserts the fd is valid for the duration
            // of the spawn; we dup immediately so nothing relies on
            // the source fd staying live past this call.
            let owned = unsafe { BorrowedFd::borrow_raw(*fd) }
                .try_clone_to_owned()
                .with_context(|| format!("dup fd {} (stdio[{}])", fd, i))?;
            match i {
                0 => {
                    cmd.stdin(Stdio::from(owned));
                }
                1 => {
                    cmd.stdout(Stdio::from(owned));
                }
                2 => {
                    cmd.stderr(Stdio::from(owned));
                }
                _ => unreachable!(),
            }
        }

        Ok(cmd)
    }

    pub(super) fn spawn(
        config: &BootstrapConfig,
        target: &Path,
        req: SpawnRequest,
    ) -> Result<Box<dyn SpawnHandle>> {
        let mut cmd = match config.proot_rootfs.as_deref() {
            None => build_base_command(target, &req.args, req.cwd.as_deref(), &req.env, req.stdio)?,
            Some(proot_rootfs) => {
                // Wrap in proot: invoke `$PREFIX/bin/proot -r <rootfs>
                // -- <target> <args...>` with stdio + env passed through.
                let proot = config.prefix.join("bin").join("proot");
                if !proot.exists() {
                    anyhow::bail!(
                        "proot mode requested but {} not found. Install proot in the bootstrap or switch to bare mode.",
                        proot.display()
                    );
                }
                let mut proot_args = vec![
                    OsString::from("-r"),
                    proot_rootfs.as_os_str().to_owned(),
                    OsString::from("-b"),
                    OsString::from("/data/data/com.zdroid/files/home:/zed"),
                    OsString::from("-b"),
                    OsString::from("/storage/emulated/0:/sdcard"),
                    OsString::from("--"),
                    target.as_os_str().to_owned(),
                ];
                proot_args.extend(req.args.iter().cloned());
                build_base_command(
                    &proot,
                    &proot_args,
                    req.cwd.as_deref(),
                    &req.env,
                    req.stdio,
                )?
            }
        };

        let child = cmd.spawn().with_context(|| {
            format!(
                "spawn {} (bootstrap mode {})",
                target.display(),
                if config.proot_rootfs.is_some() { "proot" } else { "bare" },
            )
        })?;

        Ok(Box::new(BootstrapSpawnHandle { child }))
    }
}

impl RuntimeProvider for BootstrapAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::Bootstrap
    }

    fn health_check(&self) -> HealthStatus {
        let bash = self.config.prefix.join("bin").join("bash");
        if !bash.exists() {
            return HealthStatus::NotInstalled {
                hint: format!(
                    "{} missing — bootstrap is not installed. Run install() to download it from {}.",
                    bash.display(),
                    self.config.release_repo,
                ),
            };
        }

        if let Some(proot_rootfs) = &self.config.proot_rootfs {
            // proot mode requires both proot and the rootfs to exist.
            let proot = self.config.prefix.join("bin").join("proot");
            if !proot.exists() {
                return HealthStatus::Misconfigured {
                    reason: format!(
                        "proot mode configured but {} not found. Install proot in the bootstrap or unset proot_rootfs.",
                        proot.display(),
                    ),
                };
            }
            if !proot_rootfs.exists() {
                return HealthStatus::Misconfigured {
                    reason: format!(
                        "proot_rootfs path {} does not exist. Either point at an installed rootfs or unset for bare mode.",
                        proot_rootfs.display(),
                    ),
                };
            }
        }

        HealthStatus::Healthy
    }

    fn install(&self, _progress: &mut dyn ProgressSink) -> anyhow::Result<()> {
        // Real install: pull the latest release tarball from the
        // bootstrap repo, verify SHA256, extract to $PREFIX. Lands in a
        // follow-up commit alongside the bootstrap repo split.
        anyhow::bail!(
            "BootstrapAdapter::install pending: bootstrap-repo split + GitHub releases download to be implemented. \
             Until then, use the bundled bootstrap that ships in the current APK (the existing termux_bootstrap.rs path)."
        )
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        // Real uninstall removes $PREFIX/{bin,lib,etc,share,...} but
        // preserves config dirs the user might care about (.config,
        // home/, projects/). Implementation lands with install().
        anyhow::bail!("BootstrapAdapter::uninstall pending: lands with install()")
    }

    #[cfg(target_os = "android")]
    fn spawn(&self, req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        let target = self.resolve_target(&req.program).ok_or_else(|| {
            anyhow::anyhow!(
                "bootstrap target {} not found in {} (looked in bin/, sbin/)",
                req.program,
                self.config.prefix.display(),
            )
        })?;
        android_impl::spawn(&self.config, &target, req)
    }

    #[cfg(not(target_os = "android"))]
    fn spawn(&self, _req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        anyhow::bail!("BootstrapAdapter::spawn is android-only")
    }

    fn environment_root(&self) -> std::path::PathBuf {
        // Bootstrap mode's Zed root is the app data dir Zed already
        // uses — `<app>/files/`. `config.prefix` is `<app>/files/usr/`
        // (the Termux-flavored prefix), so `.parent()` walks up to
        // `<app>/files/`. That keeps existing bootstrap users' state
        // (settings, db, themes, installed extensions, LSP downloads)
        // exactly where they were before per-adapter isolation, so
        // upgrading the APK doesn't orphan their workspace.
        self.config
            .prefix
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.config.prefix.clone())
    }

    fn list_binaries(&self) -> Vec<String> {
        // Walk bootstrap's bin dirs. Same shape as the chroot
        // adapter's walk, just different roots — bootstrap doesn't
        // have a `/usr/` layout, it puts everything directly under
        // `$PREFIX/bin/` (Termux-flavored prefix). `sbin` is rarely
        // populated on Termux but we look anyway.
        let mut names: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for sub in ["bin", "sbin"] {
            let dir = self.config.prefix.join(sub);
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str()
                            && !name.starts_with('.')
                        {
                            names.insert(name.to_string());
                        }
                    }
                }
                Err(e) => {
                    log::debug!(
                        "BootstrapAdapter::list_binaries: skipping {} ({})",
                        dir.display(),
                        e,
                    );
                }
            }
        }
        names.into_iter().collect()
    }
}
