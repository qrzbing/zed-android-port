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
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
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

        // Translate the program name AND every argv entry. Zed launches
        // LSPs / DAPs / formatters with absolute paths under env-aware
        // dirs (languages_dir, extensions_dir, …) which live under the
        // bind-mount source; without translation those bytes name a
        // path that doesn't exist inside the chroot, and the chrooted
        // child fails with `MODULE_NOT_FOUND` / ENOENT. Programs are
        // commonly short names like `node` that don't match any
        // APP_HOMES prefix and pass through unchanged; the helper is
        // a no-op in that case.
        let program_translated = {
            let prog_os = OsString::from(&req.program);
            let translated = translate_arg_for_chroot(config, &prog_os);
            translated.into_string().unwrap_or(req.program.clone())
        };
        let args_translated: Vec<OsString> = req
            .args
            .iter()
            .map(|a| translate_arg_for_chroot(config, a))
            .collect();

        let prog_bytes = program_translated.as_bytes();
        let cwd_bytes: Vec<u8> = cwd_translated
            .as_deref()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_default();

        let argc = args_translated.len() as u32;
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

        // argv: each entry length-prefixed. Translated above.
        for arg in &args_translated {
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

    /// Host-side path prefixes that map onto `config.home_bind` inside
    /// the chroot. Listed longest-first so multi-segment matches win
    /// (e.g. `/data/data/com.zdroid/files/home/foo` matches the
    /// `…/files/home` entry, not the shorter `…/files`).
    ///
    /// Both `/data/data/<pkg>` and `/data/user/0/<pkg>` are listed
    /// because Android exposes the same inode at both paths (the
    /// kernel resolves the symlink on its own, but our byte-level
    /// string compare doesn't).
    const APP_HOMES: &[&str] = &[
        "/data/data/com.zdroid/files/home",
        "/data/user/0/com.zdroid/files/home",
        "/data/data/com.zdroid/files",
        "/data/user/0/com.zdroid/files",
    ];

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

    /// Rewrite a single argv/env byte string so any host-side absolute
    /// path that starts at one of [`APP_HOMES`] gets the prefix
    /// replaced with `config.home_bind`. Used to translate paths Zed
    /// embeds in its spawn arguments (e.g. `node /data/data/com.zdroid/
    /// files/home/.zed-env/chroot/languages/<lsp>/.../cli.js`) so the
    /// same bytes name a valid path inside the chroot.
    ///
    /// Conservative on purpose:
    ///
    ///   - Only matches when the bytes START with a prefix (so
    ///     positional path args translate; mid-string occurrences in
    ///     log messages / labels / glob patterns are left alone).
    ///   - Returns the input unchanged when no prefix matches. Common
    ///     for short args like `--verbose` or env-var-like tokens.
    ///   - Operates on raw bytes via OsStr/OsString to preserve any
    ///     non-UTF-8 path components (Android paths are usually UTF-8
    ///     but we don't assume).
    pub(super) fn translate_arg_for_chroot(
        config: &ChrootConfig,
        arg: &OsString,
    ) -> OsString {
        let bytes = arg.as_bytes();
        for prefix in APP_HOMES {
            let prefix_bytes = prefix.as_bytes();
            // Must match at start AND the next byte (if any) must be
            // a path separator OR end-of-string. Otherwise `/data/data/
            // com.zdroid/files/home2` would incorrectly match the
            // `…/files/home` prefix.
            if bytes.len() >= prefix_bytes.len()
                && &bytes[..prefix_bytes.len()] == prefix_bytes
                && bytes
                    .get(prefix_bytes.len())
                    .map_or(true, |b| *b == b'/')
            {
                let target = config.home_bind.as_os_str().as_bytes();
                let suffix = &bytes[prefix_bytes.len()..];
                let mut out = Vec::with_capacity(target.len() + suffix.len());
                out.extend_from_slice(target);
                out.extend_from_slice(suffix);
                return OsString::from_vec(out);
            }
        }
        arg.clone()
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

    fn environment_root(&self) -> std::path::PathBuf {
        // Host-side path that becomes Zed's ENTIRE data root when this
        // adapter is active (config, db, logs, extensions, languages,
        // themes — everything). Two requirements:
        //
        //   1. Must live inside the bind-mount source so the same bytes
        //      are reachable inside the chroot. The daemon binds
        //      `/data/data/com.zdroid/files/home` onto `/zed`, so a
        //      file at `<this>/extensions/foo` on host is visible at
        //      `/zed/.zed-env/chroot/extensions/foo` inside the chroot.
        //      The adapter's argv-translation rewrites host paths in
        //      spawn arguments to their chroot-target equivalents so
        //      LSPs and other tools resolve cleanly.
        //
        //   2. Must be a per-adapter subdir, not just `$HOME` itself.
        //      The user's home dir is theirs, not Zdroid's; we
        //      shouldn't drop `languages/`, `extensions/`, `db/` etc.
        //      tree-roots at the top level of their home.
        //
        // Hardcoded to `/data/data/com.zdroid/files/home` rather than
        // reading ChrootConfig.home_bind because the bind-mount source
        // is itself hardcoded in zd-spawnd.c. The two MUST agree — if
        // you change one, change both. Future: thread through config
        // so both ends can be configured by the user.
        std::path::PathBuf::from(
            "/data/data/com.zdroid/files/home/.zed-env/chroot",
        )
    }

    fn list_binaries(&self) -> Vec<String> {
        // Walk the chroot rootfs's bin dirs and return every entry
        // name. The boot process turns each into a
        // `$PREFIX/zd-runtime/<name>` symlink to `zd-exec`, which is
        // how Zed's `Command::new("java")` PATH lookup finds the
        // chroot's java (or apt-get, or rust-analyzer, or whatever)
        // through the bridge.
        //
        // Order matters in PATH-lookup-equivalent style, but since
        // every entry routes to the same `zd-exec` and `zd-exec`
        // doesn't care about discovery order, we collapse duplicates
        // into a set. The chroot's own internal PATH order applies on
        // the daemon side when execvpe resolves the binary.
        let mut names: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for sub in ["usr/bin", "usr/local/bin", "usr/sbin", "bin", "sbin"] {
            let dir = self.config.root.join(sub);
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
                        "ChrootAdapter::list_binaries: skipping {} ({})",
                        dir.display(),
                        e,
                    );
                }
            }
        }
        names.into_iter().collect()
    }
}
