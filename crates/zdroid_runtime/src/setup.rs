//! Install / setup lifecycle types.
//!
//! Each adapter's `install()` is conceptually a small wizard the user
//! steps through (or watches): "check prerequisites", "download asset",
//! "verify checksum", "extract", "configure", "ping". The settings UI
//! renders these as a checklist with current-step highlight; tests use
//! the same shapes for assertions.

use serde::{Deserialize, Serialize};

/// Discrete step inside an adapter's `install()` flow. Adapters declare
/// their step list ahead of time so the UI can render the checklist
/// before any work starts. Steps run sequentially; failure of step N
/// stops the flow with that step's error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStep {
    /// Stable identifier (snake_case). Settings UI uses this as the
    /// key to track current-step / completed-step state.
    pub id: String,
    /// User-facing label ("Download bootstrap tarball").
    pub label: String,
    /// Human-readable description shown when the user expands the step
    /// row in the UI ("Pulls bootstrap-aarch64-2026.05.06-r2.tar.zst
    /// from github.com/Dylanmurzello/zdroid-bootstrap, ~80 MiB").
    pub description: Option<String>,
}

/// Outcome of an install/uninstall run. Settings UI translates this
/// into a top-level "Installed", "Already installed", "Failed (reason)"
/// banner.
#[derive(Debug, Clone)]
pub enum SetupOutcome {
    /// Install ran end-to-end and the adapter is now healthy.
    Installed,
    /// Install was a no-op because the adapter was already at the
    /// target version. Common when health-check already returned
    /// `Healthy` and the user clicked "Reinstall" anyway.
    AlreadyInstalled,
    /// Install failed at the named step. The adapter's `install()`
    /// returns `Err(anyhow::Error)` carrying the cause; this variant
    /// is the UI-friendly summary.
    Failed {
        /// `InstallStep::id` of the failing step.
        failed_step: String,
        /// One-line user-facing reason; the full error chain is
        /// captured in app logs.
        reason: String,
    },
}
