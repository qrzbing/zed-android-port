//! Android-only: inline runtime-adapter picker rendered as a settings
//! sub-page.
//!
//! Earlier iteration of this page used an `ActionLink` that dispatched
//! the existing `zdroid_runtime::PickRuntime` modal via
//! `with_active_or_new_workspace`. That worked logically but produced
//! bad UX: the modal pops on the WORKSPACE window, not the SETTINGS
//! window. On Android settings is its own Activity-style window on top
//! of the workspace, so the modal appeared BEHIND the settings UI and
//! was effectively invisible. Tapping "Open picker" looked like a
//! no-op.
//!
//! Rendering inline as a `SubPageLink` keeps the picker inside the
//! settings window itself — same render layer as every other settings
//! sub-page (Feature Flags, Tool Permissions, etc.). The button-to-
//! adapter wiring is identical to the onboarding-basics-page section
//! (`crates/onboarding/src/basics_page.rs::render_android_runtime_section`);
//! both write `runtime.toml` directly via `RuntimeFile::save`.

use gpui::{ScrollHandle, prelude::*};
use ui::{
    IconName, ToggleButtonGroup, ToggleButtonGroupSize, ToggleButtonGroupStyle,
    ToggleButtonWithIcon, prelude::*,
};
use zdroid_runtime::{RuntimeId, config::RuntimeFile};

use crate::SettingsWindow;

/// Same path the onboarding section writes to. Hardcoded — must agree
/// with `zd-exec`'s read path (`crates/zdroid_runtime/src/bin/zd-exec.rs`)
/// and the onboarding form.
const RUNTIME_TOML_PATH: &str = "/data/data/com.zdroid/files/usr/etc/zd-runtime.toml";

pub(crate) fn render_android_runtime_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    _window: &mut Window,
    _cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let current = RuntimeFile::load(std::path::Path::new(RUNTIME_TOML_PATH))
        .ok()
        .flatten()
        .map(|f| f.runtime.kind);

    let selected_index = match current {
        Some(RuntimeId::Chroot) => Some(0),
        Some(RuntimeId::Bootstrap) => Some(1),
        Some(RuntimeId::ExternalTermux) => Some(2),
        None => None,
    };

    v_flex()
        .id("android-runtime-page")
        .min_w_0()
        .size_full()
        .pt_2p5()
        .px_8()
        .pb_16()
        .gap_4()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .child(
            Label::new(
                "Select which userland Zdroid routes spawned commands into. \
                 The integrated terminal, language servers, formatters, and \
                 any tool launched from the editor go through the selected \
                 adapter. Restart Zdroid for the change to take effect.",
            )
            .size(LabelSize::Small)
            .color(Color::Muted),
        )
        .child(
            ToggleButtonGroup::single_row(
                "android_runtime_selection_settings",
                [
                    ToggleButtonWithIcon::new(
                        "Kali chroot",
                        IconName::Terminal,
                        |_, _, _| write_runtime(RuntimeId::Chroot),
                    ),
                    ToggleButtonWithIcon::new(
                        "Bootstrap",
                        IconName::Box,
                        |_, _, _| write_runtime(RuntimeId::Bootstrap),
                    ),
                    ToggleButtonWithIcon::new(
                        "Termux app",
                        IconName::Server,
                        |_, _, _| write_runtime(RuntimeId::ExternalTermux),
                    ),
                ],
            )
            .when_some(selected_index, |this, idx| this.selected_index(idx))
            .full_width()
            .size(ToggleButtonGroupSize::Medium)
            .style(ToggleButtonGroupStyle::Outlined),
        )
        .into_any_element()
}

fn write_runtime(id: RuntimeId) {
    let path = std::path::PathBuf::from(RUNTIME_TOML_PATH);
    let file = RuntimeFile::with_defaults(id);
    match file.save(&path) {
        Ok(()) => log::info!(
            "settings_ui::android_runtime: selected {:?} -> wrote {}; restart Zdroid to apply",
            id,
            path.display()
        ),
        Err(err) => log::error!(
            "settings_ui::android_runtime: failed to write {}: {:#}",
            path.display(),
            err
        ),
    }
}
