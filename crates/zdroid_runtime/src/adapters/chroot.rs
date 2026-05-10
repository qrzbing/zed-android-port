//! Chroot adapter — talks to `zd-spawnd` over a Unix socket.
//!
//! Per-spawn cost: one connect (~1ms) + one sendmsg with `SCM_RIGHTS`
//! (~1ms) + the daemon's fork/chroot/exec (~3ms). About 5ms total
//! versus ~200ms for the Magisk-su-mediated path, plus no serialized
//! queue contention at scale.
//!
//! See `crates/gpui_android/native/zd-spawnd/PROTOCOL.md` for the wire
//! format. tl;dr: header with magic + version + flags + lengths,
//! followed by prog/cwd/argv/envp byte streams, then a final
//! `sendmsg` with one dummy byte + `SCM_RIGHTS` carrying stdio fds.

use crate::config::{ChrootConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

/// GitHub releases page for the `zdroid-spawnd` Magisk module — the
/// thing the chroot adapter needs running to function. Surfaced in
/// health-check hints and in the runtime-picker UI when the daemon
/// socket is missing, so the user has a one-click path from "this
/// adapter doesn't work" to "here's how to fix it".
///
/// `releases/latest` (not a pinned tag) so we don't bit-rot every time
/// the module ships a patch. Magisk Manager's own update mechanism
/// (`updateJson` in `module.prop`) handles in-place upgrades after
/// first install.
pub const SPAWND_RELEASE_URL: &str =
    "https://github.com/Dylanmurzello/zed-android-port/releases/latest";

/// Wire-protocol magic; matches `MAGIC` in `zd-spawnd.c`. ASCII "ZDSP"
/// little-endian.
#[allow(dead_code)]
const MAGIC: u32 = 0x5A445350;

/// Wire-protocol version. Bumped only on incompatible changes.
#[allow(dead_code)]
const VERSION: u32 = 1;

/// `flags` bit 0: this spawn is replacing an interactive terminal,
/// daemon should `setsid + TIOCSCTTY` so the inner shell can do job
/// control. Non-tty spawns (LSPs, git) leave this clear.
#[allow(dead_code)]
const FLAG_INTERACTIVE: u32 = 1 << 0;

/// Connects to `zd-spawnd` per spawn. The daemon holds the elevated
/// chroot context so we skip Magisk's su mediation queue.
pub struct ChrootAdapter {
    // Stored unconditionally so the field is always part of the struct
    // shape; only the android-cfg paths actually read it. The dead-code
    // lint is suppressed for non-android builds where install/spawn are
    // unimplemented.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    config: ChrootConfig,
}

impl ChrootAdapter {
    pub fn new(config: ChrootConfig) -> anyhow::Result<Self> {
        Ok(Self { config })
    }
}

#[cfg(target_os = "android")]
mod android_impl {
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    use anyhow::{Context, Result};
    use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

    use crate::config::ChrootConfig;
    use crate::port::{SpawnHandle, SpawnRequest};

    use super::{FLAG_INTERACTIVE, MAGIC, VERSION};

    pub(super) fn connect(socket: &Path) -> Result<UnixStream> {
        UnixStream::connect(socket)
            .with_context(|| format!("connect zd-spawnd at {}", socket.display()))
    }

    pub(super) fn send_request(
        conn: &mut UnixStream,
        config: &ChrootConfig,
        req: &SpawnRequest,
    ) -> Result<()> {
        // Translate cwd from host space to chroot space. The editor
        // reports cwd in the Android sandbox (/data/data/com.zdroid/
        // files/...), but inside the chroot that path doesn't exist.
        // home_bind is where Zdroid's home lands; map it. Done before
        // sanitize_env so we can also export it as $INIT_PWD, which
        // NetHunter's /etc/profile.d/init-pwd.sh hooks to cd into.
        let cwd_translated = translate_cwd_for_chroot(config, req.cwd.as_deref());

        // Sanitize env at the chroot boundary. Zed runs with Android-
        // sandbox-specific env (PATH=$PREFIX/zd-runtime:..., HOME=
        // /data/data/..., TERMUX__*, PREFIX, etc.) — none of those
        // paths exist inside the chroot, so passing them through means
        // PATH lookup for `bash` finds nothing, the child silently
        // exec-fails with 127, and the user sees a dead PTY.
        //
        // Replace with a chroot-native baseline: standard kali PATH,
        // HOME pointing at the bind-mounted home, common display vars
        // preserved from the caller.
        let env = sanitize_env_for_chroot(config, &req.env, cwd_translated.as_deref());

        let prog_bytes = req.program.as_bytes();
        let cwd_bytes: Vec<u8> = cwd_translated
            .as_deref()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_default();

        let argc = req.args.len() as u32;
        let envc = env.len() as u32;
        let flags = if req.interactive { FLAG_INTERACTIVE } else { 0 };

        // Header: 7 × u32 little-endian.
        let header = [
            MAGIC,
            VERSION,
            flags,
            prog_bytes.len() as u32,
            cwd_bytes.len() as u32,
            argc,
            envc,
        ];
        for word in header {
            conn.write_all(&word.to_le_bytes())?;
        }

        // prog + cwd as raw bytes (no length prefix; lengths are in header).
        conn.write_all(prog_bytes)?;
        conn.write_all(&cwd_bytes)?;

        // argv: each entry length-prefixed.
        for arg in &req.args {
            let bytes = arg.as_bytes();
            conn.write_all(&(bytes.len() as u32).to_le_bytes())?;
            conn.write_all(bytes)?;
        }

        // envp: KEY=VALUE strings, length-prefixed.
        for (key, value) in &env {
            let entry = encode_env_entry(key, value);
            conn.write_all(&(entry.len() as u32).to_le_bytes())?;
            conn.write_all(&entry)?;
        }

        // Stdio fds via SCM_RIGHTS. The daemon's recvmsg expects
        // exactly one byte of regular data accompanying the ancillary,
        // so we send a single dummy byte alongside.
        let dummy = [0u8; 1];
        let iov = [std::io::IoSlice::new(&dummy)];
        let cmsgs = [ControlMessage::ScmRights(&req.stdio)];
        sendmsg::<()>(
            conn.as_raw_fd(),
            &iov,
            &cmsgs,
            MsgFlags::empty(),
            None,
        )
        .context("sendmsg with SCM_RIGHTS for stdio fds")?;

        Ok(())
    }

    /// Build the on-wire `KEY=VALUE` byte sequence. Values may contain
    /// arbitrary bytes (paths with non-UTF8 names, etc.); we don't
    /// re-encode them.
    fn encode_env_entry(key: &str, value: &OsString) -> Vec<u8> {
        let value_bytes = value.as_bytes();
        let mut buf = Vec::with_capacity(key.len() + 1 + value_bytes.len());
        buf.extend_from_slice(key.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(value_bytes);
        buf
    }

    /// Build the env that the chrooted child will run under. Pulls a
    /// small allow-list of display-related vars from the caller (TERM,
    /// COLORTERM, LANG, …) and pins everything else to chroot-native
    /// defaults. Anything Android-sandbox-specific (PATH pointing at
    /// $PREFIX/bin/, TERMUX__*, ZED_*) is dropped — those paths don't
    /// resolve inside the rootfs and would silently break exec / shell
    /// startup.
    ///
    /// Sets HOME=/root and USER=root explicitly. Empirical finding:
    /// bash does NOT do `getpwuid(uid)` to fill HOME on its own — it
    /// expects HOME to be in the inherited env, the way login(1) /
    /// sshd / a desktop session manager would set it. With HOME unset,
    /// `~/.local/bin` in the chrooted .profile expands to `/.local/bin`
    /// which never exists, so user-installed tools (claude, pip --user
    /// installs, cargo binaries) silently disappear from PATH. /root
    /// is hardcoded because this adapter is debian-rootfs-shaped: uid 0
    /// → /root in /etc/passwd. (Don't set HOME=home_bind — that would
    /// make bash source `<home_bind>/.bashrc` instead of
    /// `/root/.bashrc`, losing the kali prompt + aliases + actual user
    /// dotfiles.)
    ///
    /// Sets `INIT_PWD=<chroot_cwd>` when a translated cwd is supplied.
    /// NetHunter ships `/etc/profile.d/init-pwd.sh` which does
    /// `cd "$INIT_PWD"` if the var is set and the dir exists; combined
    /// with the customize.sh patch that gates `/root/.bash_profile`'s
    /// `cd /root` and `cd ~` on the same var, this is what makes a
    /// chrooted login shell actually land at the project path instead
    /// of bouncing to /root.
    pub(super) fn sanitize_env_for_chroot(
        _config: &ChrootConfig,
        caller_env: &std::collections::HashMap<String, OsString>,
        init_pwd: Option<&Path>,
    ) -> Vec<(String, OsString)> {
        // Display / locale vars worth carrying across the boundary so
        // the inner shell renders correctly. Add to this list cautiously
        // — anything path-shaped is an exec-failure waiting to happen.
        const PASSTHROUGH: &[&str] = &[
            "TERM",
            "COLORTERM",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "DISPLAY",
        ];

        let mut env: Vec<(String, OsString)> = Vec::new();

        // Bootstrap PATH so `execvpe(bash)` succeeds. Bash itself, on
        // interactive startup, sources `/etc/profile` and `~/.bashrc`,
        // which typically PREPEND user-installed tool dirs (e.g.
        // `~/.npm-global/bin`, `~/.cargo/bin`) — those win over our
        // bootstrap PATH for any binary the user installed via npm /
        // cargo / etc. Matches what `getconf PATH` returns in fresh
        // debian.
        env.push((
            "PATH".to_string(),
            OsString::from("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        ));

        // HOME / USER: see docstring. Fixed values for the debian-style
        // rootfs the chroot adapter targets.
        env.push(("HOME".to_string(), OsString::from("/root")));
        env.push(("USER".to_string(), OsString::from("root")));
        env.push(("LOGNAME".to_string(), OsString::from("root")));

        // Carry display vars across.
        for key in PASSTHROUGH {
            if let Some(value) = caller_env.get(*key) {
                env.push((key.to_string(), value.clone()));
            }
        }

        // INIT_PWD: read by NetHunter's /etc/profile.d/init-pwd.sh.
        // The patched .bash_profile also gates its own `cd /root` /
        // `cd ~` on this — set means "Zdroid asked for a specific
        // landing dir, leave it alone".
        if let Some(pwd) = init_pwd {
            env.push(("INIT_PWD".to_string(), pwd.as_os_str().to_owned()));
        }

        env
    }

    /// Translate a host-space cwd to chroot-space. The chroot's
    /// `home_bind` directory bind-mounts the editor's home, so any path
    /// rooted in the host's app sandbox lands at `home_bind/<rel>`
    /// inside the chroot. Other paths fall back to home_bind so the
    /// shell starts somewhere sensible (vs `/` which is bare).
    pub(super) fn translate_cwd_for_chroot(
        config: &ChrootConfig,
        host_cwd: Option<&Path>,
    ) -> Option<std::path::PathBuf> {
        let host_cwd = host_cwd?;

        // Common Android sandbox prefixes the editor's PWD might use.
        // Both forms point at the same dir; the kernel resolves the
        // symlink for us, but our string compare needs both branches.
        const APP_HOMES: &[&str] = &[
            "/data/data/com.zdroid/files/home",
            "/data/user/0/com.zdroid/files/home",
            "/data/data/com.zdroid/files",
            "/data/user/0/com.zdroid/files",
        ];

        for prefix in APP_HOMES {
            if let Ok(rel) = host_cwd.strip_prefix(prefix) {
                let mut translated = config.home_bind.clone();
                if !rel.as_os_str().is_empty() {
                    translated.push(rel);
                }
                return Some(translated);
            }
        }

        // No match — likely a path the user opened outside the home.
        // Fall back to home_bind so the shell starts somewhere readable.
        Some(config.home_bind.clone())
    }

    /// Read the daemon's `response_spawned` (4 × u32). Returns the
    /// negotiated child PID (daemon-internal) or surfaces an error
    /// reflecting the daemon's reported errno.
    pub(super) fn read_spawned_response(conn: &mut UnixStream) -> Result<u32> {
        let mut buf = [0u8; 16];
        conn.read_exact(&mut buf)
            .context("read response_spawned from daemon")?;
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let status = i32::from_le_bytes(buf[8..12].try_into().unwrap());
        let pid = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        if magic != MAGIC {
            anyhow::bail!("response_spawned bad magic: {magic:#x}");
        }
        if version != VERSION {
            anyhow::bail!("response_spawned version mismatch: {version}");
        }
        if status < 0 {
            let errno = -status;
            anyhow::bail!(
                "daemon refused spawn: errno={} ({})",
                errno,
                nix::errno::Errno::from_raw(errno).desc(),
            );
        }
        Ok(pid)
    }

    /// Read `response_exited` (3 × u32). Returns the exit code, with
    /// negative values representing terminating signals (e.g. -9 for
    /// SIGKILL).
    pub(super) fn read_exited_response(conn: &mut UnixStream) -> Result<i32> {
        let mut buf = [0u8; 12];
        conn.read_exact(&mut buf)
            .context("read response_exited from daemon")?;
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let exit_code = i32::from_le_bytes(buf[8..12].try_into().unwrap());
        if magic != MAGIC {
            anyhow::bail!("response_exited bad magic: {magic:#x}");
        }
        if version != VERSION {
            anyhow::bail!("response_exited version mismatch: {version}");
        }
        Ok(exit_code)
    }

    pub(super) struct ChrootSpawnHandle {
        pub conn: UnixStream,
        /// Cached on first `wait()` call so subsequent calls are
        /// idempotent and don't try a second `read_exited_response`.
        pub cached_exit_code: Option<i32>,
    }

    impl SpawnHandle for ChrootSpawnHandle {
        fn wait(&mut self) -> Result<i32> {
            if let Some(code) = self.cached_exit_code {
                return Ok(code);
            }
            let code = read_exited_response(&mut self.conn)?;
            self.cached_exit_code = Some(code);
            Ok(code)
        }

        fn kill(&mut self) -> Result<()> {
            // Per protocol: shutdown the write half. Daemon reads 0,
            // SIGKILLs the child, sends `response_exited` with -9.
            self.conn
                .shutdown(std::net::Shutdown::Write)
                .context("shutdown write half to signal kill")
        }
    }

    pub(super) fn spawn(
        config: &ChrootConfig,
        req: SpawnRequest,
    ) -> Result<Box<dyn SpawnHandle>> {
        let mut conn = connect(&config.spawnd_socket)?;
        send_request(&mut conn, config, &req)?;
        let _pid = read_spawned_response(&mut conn)?;
        Ok(Box::new(ChrootSpawnHandle {
            conn,
            cached_exit_code: None,
        }))
    }

    pub(super) fn health_check(config: &ChrootConfig) -> super::HealthStatus {
        match UnixStream::connect(&config.spawnd_socket) {
            Ok(_) => super::HealthStatus::Healthy,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                super::HealthStatus::NotInstalled {
                    hint: format!(
                        "zd-spawnd daemon not running. Install the Magisk module from {}, then reboot.",
                        super::SPAWND_RELEASE_URL,
                    ),
                }
            }
            Err(e) => super::HealthStatus::Failed {
                error: format!("connect zd-spawnd at {}: {e}", config.spawnd_socket.display()),
            },
        }
    }

}

impl RuntimeProvider for ChrootAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::Chroot
    }

    #[cfg(target_os = "android")]
    fn health_check(&self) -> HealthStatus {
        android_impl::health_check(&self.config)
    }

    #[cfg(not(target_os = "android"))]
    fn health_check(&self) -> HealthStatus {
        HealthStatus::NotInstalled {
            hint: "ChrootAdapter is android-only; this is a non-android build.".into(),
        }
    }

    fn install(&self, _progress: &mut dyn ProgressSink) -> anyhow::Result<()> {
        anyhow::bail!(
            "ChrootAdapter::install: install the zdroid-spawnd Magisk module \
             to start the daemon. Auto-install of Magisk modules from within \
             the app is not yet supported."
        )
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        // No-op by design. The chroot rootfs is the user's; Magisk
        // module removal happens through Magisk Manager.
        Ok(())
    }

    #[cfg(target_os = "android")]
    fn spawn(&self, req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        android_impl::spawn(&self.config, req)
    }

    #[cfg(not(target_os = "android"))]
    fn spawn(&self, _req: SpawnRequest) -> anyhow::Result<Box<dyn SpawnHandle>> {
        anyhow::bail!("ChrootAdapter::spawn is android-only")
    }
}
