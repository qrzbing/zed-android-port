use std::ffi::OsStr;
#[cfg(not(target_os = "macos"))]
use std::path::Path;
#[cfg(target_os = "android")]
use std::path::PathBuf;
#[cfg(target_os = "android")]
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "macos")]
pub use darwin::{Child, Command, Stdio};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000_u32;

/// Path to the `zd-exec` binary the bridge dispatches through. zd-exec
/// itself reads `zd-runtime.toml`, picks the active adapter, and routes
/// the spawn into the chroot / bootstrap / external-Termux world. We
/// invoke it by short name so kernel PATH lookup finds the
/// `$PREFIX/zd-runtime/zd-exec` symlink first (matching how every other
/// PATH-resolved spawn enters the bridge) instead of hardcoding
/// `$PREFIX/bin/zd-exec` — short name keeps this lib free of app-package
/// assumptions.
#[cfg(target_os = "android")]
const ZD_EXEC_PROGRAM: &str = "zd-exec";

/// Active adapter's host-side environment root. Set once at boot from
/// `lib.rs` (after `RuntimeProvider::environment_root()` is known); any
/// absolute-path spawn whose program lives under this root is rewritten
/// to route through `zd-exec` so it lands inside the right userland.
///
/// `OnceLock` so the slot is initialized exactly once and reads are
/// lock-free for every subsequent `Command::new`.
#[cfg(target_os = "android")]
static ENVIRONMENT_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Called from `lib.rs` after the runtime adapter is resolved.
/// Subsequent `Command::new` calls will detect absolute paths under
/// `root` and rewrite them to `zd-exec <abs_path> <args…>`.
///
/// Idempotent in spirit but enforced by `OnceLock`: a second call after
/// the slot is set is a no-op. Don't try to "switch adapter at runtime"
/// by re-registering — adapter switches require a restart so paths /
/// settings / extensions get re-read from the new root.
#[cfg(target_os = "android")]
pub fn register_environment_root(root: PathBuf) {
    if ENVIRONMENT_ROOT.set(root).is_err() {
        log::debug!(
            "util::command: environment_root already registered; ignoring re-register"
        );
    }
}

pub fn new_command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[cfg(target_os = "windows")]
pub fn new_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
pub fn new_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

#[cfg(target_os = "android")]
pub fn new_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    // Same env_root bridge + shebang fixup as `Command::new` below;
    // mirrored here so synchronous spawn sites get the rewrite too.
    // See those fns for the full rationale.
    let program = program.as_ref();
    if let Some(program_path) = env_root_program_path(program) {
        let mut cmd = std::process::Command::new(ZD_EXEC_PROGRAM);
        cmd.arg(program_path);
        return cmd;
    }
    match detect_env_shebang(program) {
        Some((interp, script)) => {
            let mut cmd = std::process::Command::new(interp);
            cmd.arg(script);
            cmd
        }
        None => std::process::Command::new(program),
    }
}

/// Android-only env-root bridge helper.
///
/// Zed-the-app runs on Android's bionic libc (the only libc the platform
/// linker loads for APK processes), but every spawn target that lives
/// under the active adapter's `environment_root()` is meant to run in
/// that adapter's userland — a glibc rootfs for the chroot adapter, a
/// Termux-flavored bionic prefix for the bootstrap adapter, etc. If we
/// let the host kernel exec a chroot-side absolute path directly, the
/// process either fails because the binary's `PT_INTERP` (`/lib/ld-
/// linux-aarch64.so.1`) doesn't exist on bionic, or because the script's
/// shebang resolves against a non-existent host `/usr/bin/env`. Either
/// way the user sees a bare "No such file or directory" error and the
/// LSP / language tool silently dies.
///
/// This helper detects the case: program is an absolute path that lives
/// strictly under the registered environment root. When it matches we
/// return `Some(program_path)` and the caller rewrites the spawn to
/// `zd-exec <program_path> <original_args…>`. `zd-exec` then reads
/// `zd-runtime.toml`, picks the active adapter, and dispatches the
/// spawn into the right userland — chroot users land inside the chroot
/// where ld-linux exists, bootstrap users land in their prefix, etc.
///
/// Cost: one read of a `OnceLock` plus a `starts_with` byte-compare on
/// every `Command::new` invocation. Cache-hot, < 1µs.
///
/// Returns `None` when `register_environment_root` was never called
/// (e.g. during init before adapter is picked), when `program` is a
/// relative path / short name (kernel PATH lookup handles those via the
/// `zd-runtime/<name>` symlinks already), or when the path doesn't live
/// under env_root (system binaries like `/system/bin/sh` keep their
/// native exec semantics).
#[cfg(target_os = "android")]
fn env_root_program_path(program: &OsStr) -> Option<PathBuf> {
    let root = ENVIRONMENT_ROOT.get()?;
    let path = Path::new(program);
    if !path.is_absolute() {
        return None;
    }
    // `starts_with` matches on full path components, so a literal
    // prefix like `<root>foo` won't false-match against `<root>/foo`.
    if path.starts_with(root) {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Android-only shebang rewrite helper.
///
/// On Android the Zed app process lives in bionic's filesystem
/// sandbox. There is no `/usr/bin/env` on host — it lives only
/// inside the user's chroot / bootstrap. So a script whose first
/// line is `#!/usr/bin/env python3` (the standard portable shebang)
/// can't be exec'd directly: kernel exec reads the shebang, tries
/// to launch `/usr/bin/env`, ENOENT, the whole spawn fails before
/// PATH lookup ever happens.
///
/// We rescue this by detecting the pattern at `Command::new` time:
/// if the program is an absolute path to a regular file whose first
/// line is `#!/usr/bin/env <interp>`, return `(interp, path_to_script)`.
/// The caller then builds the Command as
/// `Command::new(interp).arg(script_path)` — the interpreter is a
/// SHORT name, kernel PATH lookup finds `zd-runtime/<interp>`, that
/// re-execs into `zd-exec`, the active runtime adapter dispatches
/// the spawn into the right environment (chroot / bootstrap), and
/// the script runs with its interpreter resolved against the
/// adapter's `/usr/bin/python3` (or whichever) inside that env.
///
/// Returns `None` for: relative-path programs (PATH lookup handles
/// them on its own), non-shebang files (ELF binaries), shebangs
/// other than `#!/usr/bin/env <X>` (rare; would need different
/// handling), and any I/O failure (we silently fall through to the
/// stock spawn behavior — the original error surfaces unchanged).
///
/// Cost: one `open(2)` + first-line read at every `Command::new`.
/// On a freshly-mmaped filesystem this is microseconds; for already
/// hot pages it's a couple syscalls. LSP spawn is a once-per-session
/// event, so even a few hundred microseconds is invisible.
#[cfg(target_os = "android")]
fn detect_env_shebang(program: &OsStr) -> Option<(std::ffi::OsString, std::path::PathBuf)> {
    use std::io::{BufRead, BufReader, Read};

    let path = Path::new(program);
    if !path.is_absolute() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    // Cap the read at a reasonable shebang length (256 bytes) so we
    // never accidentally slurp a huge binary into memory just to find
    // out it doesn't have a shebang.
    let mut reader = BufReader::new(file.take(256));
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let first = first.trim_end();

    let rest = first.strip_prefix("#!/usr/bin/env ")?;
    // `#!/usr/bin/env -S python3 -u` (rare GNU extension) leaves the
    // first whitespace-split token as `-S`; ignore those forms — we'd
    // need full shebang arg parsing to handle them correctly and the
    // chrooted env's kernel will itself handle them if we route there.
    let interp = rest.split_whitespace().next()?;
    if interp.starts_with('-') {
        return None;
    }

    Some((std::ffi::OsString::from(interp), path.to_path_buf()))
}

#[cfg(not(target_os = "macos"))]
pub type Child = smol::process::Child;

#[cfg(not(target_os = "macos"))]
pub use std::process::Stdio;

#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct Command(smol::process::Command);

#[cfg(not(target_os = "macos"))]
impl Command {
    #[inline]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        #[cfg(target_os = "windows")]
        {
            use smol::process::windows::CommandExt;
            let mut cmd = smol::process::Command::new(program);
            cmd.creation_flags(CREATE_NO_WINDOW);
            Self(cmd)
        }
        #[cfg(target_os = "android")]
        {
            // Two rewrites, in this order:
            //
            //   1. env-root bridge — if `program` is an absolute path
            //      under the active adapter's `environment_root()`,
            //      rewrite to `zd-exec <program>`. zd-exec dispatches
            //      the spawn into the configured runtime (chroot /
            //      bootstrap / external Termux), so glibc-linked
            //      binaries from inside a chroot rootfs run where
            //      their loader actually exists. See
            //      `env_root_program_path` for the full rationale.
            //
            //   2. shebang rewrite for absolute-path script invocations
            //      that need `/usr/bin/env`. See `detect_env_shebang`
            //      for the full rationale; tl;dr the host has no
            //      `/usr/bin/env`, so we replace the program with the
            //      script's declared interpreter and let kernel PATH
            //      lookup route through `zd-runtime/<interp>` into the
            //      chroot/bootstrap that does have it.
            //
            // The env-root bridge wins when both could apply (e.g. a
            // script that lives inside env_root): routing through
            // zd-exec is the more general fix and the chroot adapter
            // will resolve shebangs natively once the script is exec'd
            // inside the rootfs.
            let program_ref = program.as_ref();
            if let Some(program_path) = env_root_program_path(program_ref) {
                let mut cmd = smol::process::Command::new(ZD_EXEC_PROGRAM);
                cmd.arg(program_path);
                return Self(cmd);
            }
            if let Some((interp, script)) = detect_env_shebang(program_ref) {
                let mut cmd = smol::process::Command::new(interp);
                cmd.arg(script);
                return Self(cmd);
            }
            Self(smol::process::Command::new(program_ref))
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
        Self(smol::process::Command::new(program))
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.0.get_args()
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> &mut Self {
        self.0.env(key, val);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.0.env_remove(key);
        self
    }

    pub fn env_clear(&mut self) -> &mut Self {
        self.0.env_clear();
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(dir);
        self
    }

    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdin(cfg.into());
        self
    }

    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stdout(cfg.into());
        self
    }

    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.0.stderr(cfg.into());
        self
    }

    pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
        self.0.kill_on_drop(kill_on_drop);
        self
    }

    pub fn spawn(&mut self) -> std::io::Result<Child> {
        self.0.spawn()
    }

    pub async fn output(&mut self) -> std::io::Result<std::process::Output> {
        self.0.output().await
    }

    pub async fn status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.status().await
    }

    pub fn get_program(&self) -> &OsStr {
        self.0.get_program()
    }
}
