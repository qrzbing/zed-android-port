//! Runtime READ/WRITE_EXTERNAL_STORAGE prompt for Android.
//!
//! At `targetSdk = 28` we land in legacy-storage mode, but
//! `READ_EXTERNAL_STORAGE` and `WRITE_EXTERNAL_STORAGE` are still dangerous
//! permissions that Android won't grant at install time — they need a
//! runtime dialog. SAF folder picking dodges this for tree access, but the
//! moment `RealFs` reads `/storage/emulated/0/projects/foo.rs` directly
//! (which is what happens after the SAF picker hands us back a
//! `/storage/...` path), the syscall fails with `EACCES` until the user has
//! granted those perms.
//!
//! This module is the JNI bridge to MainActivity's `requestStoragePermissions()`,
//! which fires the dialog on first launch. Fire-and-forget by design — if
//! the user denies, file ops EACCES with a clean error and they can grant
//! later via Settings → Apps → zed_android → Permissions.
//!
//! The MANAGE_EXTERNAL_STORAGE escape hatch we used at `targetSdk=35` is
//! deliberately not part of this flow. That permission is API 30+ only and
//! requires a Settings deep-link rather than a runtime dialog; at
//! `targetSdk=28` it has no effect anyway.

use std::sync::OnceLock;

use android_activity::AndroidApp;
use anyhow::{Context, Result};
use jni::{JavaVM, objects::JObject};

static REQUESTED: OnceLock<()> = OnceLock::new();

/// Fire the `requestStoragePermissions()` JNI call once per process. Logs
/// the result code (1 = already granted, 0 = dialog posted). Re-entry from
/// activity recreation is a no-op via `OnceLock`.
pub fn request_once(android_app: &AndroidApp) {
    if REQUESTED.get().is_some() {
        return;
    }
    match request_inner(android_app) {
        Ok(code) => {
            log::info!(
                "storage: requestStoragePermissions returned {} ({})",
                code,
                if code == 1 { "already granted" } else { "dialog posted" }
            );
            let _ = REQUESTED.set(());
        }
        Err(err) => {
            // Don't latch the OnceLock on failure — let the next android_main
            // re-entry try again. Failures here are usually JNI thread-attach
            // races during early boot and recover after lifecycle settles.
            log::warn!("storage: requestStoragePermissions failed: {err:#}");
        }
    }
}

fn request_inner(android_app: &AndroidApp) -> Result<i32> {
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm
        .attach_current_thread()
        .context("attach_current_thread for storage permissions")?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };
    let result = env
        .call_method(&activity, "requestStoragePermissions", "()I", &[])
        .context("MainActivity.requestStoragePermissions")?
        .i()?;
    Ok(result)
}
