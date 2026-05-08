//! Runtime adapter picker — lets the user pick which userland Zdroid
//! routes its spawns through (chroot, bootstrap, external Termux).
//!
//! Surfaces as a centered modal triggered by the `zdroid: pick runtime`
//! action. Mirrors Zed's welcome-page card aesthetic.
//!
//! v1 scope (this commit):
//!   - Render three cards with name, tagline, live health snapshot.
//!   - Select button per card. Click logs the choice + dismisses.
//!     Persistence to `runtime.toml` lands once we've nailed down
//!     where Zdroid stores adapter config (likely `$PREFIX/etc/zd-runtime.toml`)
//!     and added the file-write helper.
//!
//! Future scope (queued in tasks #33/#34):
//!   - Install / Uninstall buttons backed by adapter `install()` paths.
//!   - "Restart Zdroid" prompt after a switch.
//!   - First-launch auto-open when no `runtime.toml` exists yet.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, Window,
    actions, prelude::*, px,
};
use theme::ActiveTheme;
use ui::{
    Button, Clickable, Color, FluentBuilder, Headline, HeadlineSize, Icon, IconName, IconSize,
    Label, LabelCommon, LabelSize, ParentElement, Styled, h_flex, v_flex,
};
use workspace::{ModalView, Workspace};
use zdroid_runtime::{
    HealthStatus, RuntimeId, RuntimeProvider,
    adapters,
    config::{BootstrapConfig, ChrootConfig, ExternalTermuxConfig},
};

actions!(
    zdroid_runtime,
    [
        /// Open the runtime adapter picker modal.
        PickRuntime,
    ]
);

/// Register the action + workspace hook. Called from `lib.rs::android_main`
/// during workspace init. Once registered, command-palette → "zdroid:
/// pick runtime" toggles the modal.
pub fn register(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace.register_action(toggle_runtime_picker);
        },
    )
    .detach();
}

fn toggle_runtime_picker(
    workspace: &mut Workspace,
    _: &PickRuntime,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.toggle_modal(window, cx, RuntimePicker::new);
}

struct AdapterEntry {
    id: RuntimeId,
    tagline: &'static str,
    health: HealthStatus,
}

pub struct RuntimePicker {
    focus_handle: FocusHandle,
    entries: Vec<AdapterEntry>,
}

impl RuntimePicker {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            entries: build_entries(),
        }
    }

    fn select(&mut self, id: RuntimeId, _window: &mut Window, cx: &mut Context<Self>) {
        // v1: log the selection, dismiss. Persistence to runtime.toml
        // + restart prompt land in the next iteration once we've
        // settled where the file lives in Zdroid's data dir.
        log::info!("zdroid_runtime_picker: user selected adapter {:?}", id);
        cx.emit(DismissEvent);
    }
}

impl ModalView for RuntimePicker {}
impl EventEmitter<DismissEvent> for RuntimePicker {}

impl Focusable for RuntimePicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RuntimePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_colors = cx.theme().colors();

        v_flex()
            .key_context("RuntimePicker")
            .track_focus(&self.focus_handle)
            .w(px(560.0))
            .max_h(px(720.0))
            .p_6()
            .gap_4()
            .bg(theme_colors.elevated_surface_background)
            .border_1()
            .border_color(theme_colors.border)
            .rounded_lg()
            // Clip card content that overflows the modal's nominal
            // width — without this, gpui flexbox lets long taglines
            // grow past the modal box and the Select button drifts
            // off into editor space.
            .overflow_hidden()
            .child(
                v_flex()
                    .gap_1()
                    .child(Headline::new("Pick your runtime").size(HeadlineSize::Medium))
                    .child(
                        Label::new(
                            "Where Zdroid runs your tools — LSPs, git, formatters, terminal. \
                             Switch any time from Settings.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .child(
                v_flex().gap_3().children(
                    self.entries
                        .iter()
                        .enumerate()
                        .map(|(idx, entry)| render_card(idx, entry, cx)),
                ),
            )
    }
}

fn render_card(
    idx: usize,
    entry: &AdapterEntry,
    cx: &mut Context<RuntimePicker>,
) -> AnyElement {
    let theme_colors = cx.theme().colors();
    let id = entry.id;
    let name = id.display_name();
    let tagline = entry.tagline;

    let (dot_color, dot_label): (Color, &'static str) = match &entry.health {
        HealthStatus::Healthy => (Color::Success, "Ready"),
        HealthStatus::NotInstalled { .. } => (Color::Muted, "Not installed"),
        HealthStatus::Misconfigured { .. } => (Color::Warning, "Needs attention"),
        HealthStatus::Failed { .. } => (Color::Error, "Failed"),
    };

    let detail = match &entry.health {
        HealthStatus::Healthy => None,
        HealthStatus::NotInstalled { hint } => Some(hint.clone()),
        HealthStatus::Misconfigured { reason } => Some(reason.clone()),
        HealthStatus::Failed { error } => Some(error.clone()),
    };

    h_flex()
        .id(("adapter-card", idx))
        .gap_4()
        .p_4()
        .w_full()
        .border_1()
        .border_color(theme_colors.border_variant)
        .rounded_md()
        // Allow inner flex children to shrink below their content
        // width (CSS `min-width: 0` equivalent) — without this the
        // long tagline labels push the layout past the modal's edge.
        .min_w_0()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Server).size(IconSize::Small))
                        .child(Headline::new(name).size(HeadlineSize::XSmall)),
                )
                .child(
                    Label::new(tagline)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::new(IconName::Circle)
                                .size(IconSize::XSmall)
                                .color(dot_color),
                        )
                        .child(
                            Label::new(dot_label)
                                .size(LabelSize::XSmall)
                                .color(dot_color),
                        ),
                )
                .when_some(detail, |this, detail| {
                    this.child(
                        Label::new(Arc::<str>::from(detail))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                }),
        )
        .child(
            Button::new(("select", idx), "Select").on_click(cx.listener(
                move |this, _, window, cx| {
                    this.select(id, window, cx);
                },
            )),
        )
        .into_any_element()
}

/// Build the per-adapter health snapshot at modal-open time. Each
/// adapter is constructed with on-device defaults so the health probe
/// can run; the user's eventual `runtime.toml` overrides these when
/// the adapter is actually selected.
fn build_entries() -> Vec<AdapterEntry> {
    let chroot_health = adapters::chroot::ChrootAdapter::new(default_chroot_config())
        .map(|a| a.health_check())
        .unwrap_or_else(|err| HealthStatus::Failed {
            error: err.to_string(),
        });
    let bootstrap_health = adapters::bootstrap::BootstrapAdapter::new(default_bootstrap_config())
        .map(|a| a.health_check())
        .unwrap_or_else(|err| HealthStatus::Failed {
            error: err.to_string(),
        });
    let termux_health =
        adapters::external_termux::ExternalTermuxAdapter::new(default_termux_config())
            .map(|a| a.health_check())
            .unwrap_or_else(|err| HealthStatus::Failed {
                error: err.to_string(),
            });

    vec![
        AdapterEntry {
            id: RuntimeId::Chroot,
            tagline:
                "Fastest. Routes through the persistent zd-spawnd daemon. Requires Magisk root + the zdroid-spawnd module.",
            health: chroot_health,
        },
        AdapterEntry {
            id: RuntimeId::Bootstrap,
            tagline:
                "Self-contained Termux-flavored userland inside Zdroid's sandbox. Bare or proot-wrapped. No root, no external app.",
            health: bootstrap_health,
        },
        AdapterEntry {
            id: RuntimeId::ExternalTermux,
            tagline:
                "Bridges to the user's installed Termux app via Intent IPC. Slowest path; uses the user's existing setup.",
            health: termux_health,
        },
    ]
}

fn default_chroot_config() -> ChrootConfig {
    ChrootConfig {
        root: PathBuf::from("/data/local/nhsystem/kali-arm64"),
        home_bind: PathBuf::from("/zed"),
        spawnd_socket: PathBuf::from("/data/data/com.zdroid/files/run/zd-spawn"),
        su_path: PathBuf::from("/product/bin/su"),
    }
}

fn default_bootstrap_config() -> BootstrapConfig {
    BootstrapConfig {
        prefix: PathBuf::from("/data/data/com.zdroid/files/usr"),
        proot_rootfs: None,
        release_repo: "Dylanmurzello/zdroid-bootstrap".into(),
    }
}

fn default_termux_config() -> ExternalTermuxConfig {
    ExternalTermuxConfig {
        package: "com.termux".into(),
        prefix: PathBuf::from("/data/data/com.termux/files/usr"),
    }
}

