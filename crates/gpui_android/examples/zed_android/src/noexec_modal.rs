//! Noexec move modal — Trust-style centered confirmation rendered when
//! the title-bar banner is tapped on a worktree that lives on a noexec
//! mount (FUSE-mounted /storage/emulated/0/* in practice).
//!
//! Mirrors `workspace::security_modal::SecurityModal` exactly:
//! `AlertModal` chrome, header with ⚠ + title + path-as-subtitle,
//! body with explanation + impact bullets + a Suppress checkbox, footer
//! with right-aligned [Cancel | Copy to ~/projects] both showing
//! `KeyBinding::for_action` shortcut hints. Primary button uses
//! `ButtonStyle::Filled` + `layer(ElevationIndex::ModalSurface)` so it
//! visibly fills against the modal surface, like production's
//! "Trust and Continue".

use std::path::PathBuf;

use gpui::{
    AppContext as _, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Window, rems,
};
use log::{error, info};
use theme::ActiveTheme;
use ui::{
    AlertModal, Button, ButtonCommon, ButtonStyle, Checkbox, Clickable, Color, ElevationIndex,
    Headline, HeadlineSize, Icon, IconName, KeyBinding, Label, LabelCommon, ListBulletItem,
    ToggleState, h_flex, rems_from_px, v_flex,
};
use workspace::{DismissDecision, ModalView, MultiWorkspace};

pub struct NoexecMoveModal {
    abs_path: PathBuf,
    /// Persisted-on-Cancel-only. If the user closes the dialog without
    /// hitting Copy and this is checked, we record the path in
    /// `~/.cache/zed-android/noexec-suppressed.json` so the banner stops
    /// rendering for it. We do NOT auto-suppress on Copy — copying moves
    /// the worktree to a non-noexec location, so the banner already
    /// won't render for the new path; suppressing the *original* path
    /// would be redundant.
    suppress: bool,
    focus_handle: FocusHandle,
}

impl NoexecMoveModal {
    pub fn new(abs_path: PathBuf, cx: &mut Context<Self>) -> Self {
        Self {
            abs_path,
            suppress: false,
            focus_handle: cx.focus_handle(),
        }
    }

    fn copy_and_dismiss(&mut self, cx: &mut Context<Self>) {
        let src = self.abs_path.clone();
        let projects_root = match gpui_android::storage::projects_dir() {
            Some(p) => p,
            None => {
                error!(
                    "noexec-modal: TERMUX__HOME unset; can't compute \
                     ~/projects destination, refusing to move"
                );
                cx.emit(DismissEvent);
                return;
            }
        };
        let basename = match src.file_name() {
            Some(n) => n.to_owned(),
            None => {
                error!(
                    "noexec-modal: src {} has no basename, refusing to move",
                    src.display()
                );
                cx.emit(DismissEvent);
                return;
            }
        };
        // Don't clobber an existing project with the same name. Suffix
        // `-imported`, `-imported-2`, etc. so the user keeps their
        // previous import and the new one lands cleanly.
        let mut dst = projects_root.join(&basename);
        if dst.exists() {
            let stem = basename.to_string_lossy().to_string();
            let mut suffix = 1usize;
            loop {
                let candidate = projects_root.join(format!(
                    "{stem}-imported{}",
                    if suffix == 1 {
                        String::new()
                    } else {
                        format!("-{suffix}")
                    }
                ));
                if !candidate.exists() {
                    dst = candidate;
                    break;
                }
                suffix += 1;
            }
        }
        info!(
            "noexec-modal: copying {} -> {}",
            src.display(),
            dst.display()
        );
        cx.spawn(async move |this, cx| {
            let dst_for_copy = dst.clone();
            let src_for_copy = src.clone();
            let copy_result = cx
                .background_spawn(async move {
                    gpui_android::storage::copy_tree(&src_for_copy, &dst_for_copy)
                })
                .await;
            match copy_result {
                Ok(bytes) => info!(
                    "noexec-modal: copied {bytes} bytes to {}",
                    dst.display()
                ),
                Err(err) => {
                    error!("noexec-modal: copy failed: {err:#}");
                    let _ = this.update(cx, |_, cx| cx.emit(DismissEvent));
                    return;
                }
            }
            let mw = cx.update(|cx| {
                cx.active_window()
                    .and_then(|w| w.downcast::<MultiWorkspace>())
            });
            if let Some(mw) = mw {
                let task = mw.update(cx, |mw, window, cx| {
                    mw.open_project(
                        vec![dst],
                        workspace::OpenMode::Activate,
                        window,
                        cx,
                    )
                });
                if let Ok(task) = task {
                    if let Err(err) = task.await {
                        error!("noexec-modal: open_project failed: {err:#}");
                    }
                }
            } else {
                error!("noexec-modal: no active MultiWorkspace to open into");
            }
            let _ = this.update(cx, |_, cx| cx.emit(DismissEvent));
        })
        .detach();
    }

    fn cancel_and_maybe_suppress(&mut self, cx: &mut Context<Self>) {
        if self.suppress {
            gpui_android::storage::add_noexec_suppressed(&self.abs_path);
        }
        cx.emit(DismissEvent);
    }
}

impl Focusable for NoexecMoveModal {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for NoexecMoveModal {}

impl ModalView for NoexecMoveModal {
    fn fade_out_background(&self) -> bool {
        true
    }

    fn on_before_dismiss(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> DismissDecision {
        DismissDecision::Dismiss(true)
    }
}

impl Render for NoexecMoveModal {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path_label = self.abs_path.display().to_string();

        AlertModal::new("zed-android-noexec-modal")
            .width(rems(40.))
            .key_context("NoexecMoveModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &menu::Confirm, _window, cx| {
                this.copy_and_dismiss(cx);
            }))
            .on_action(cx.listener(|this, _: &menu::Cancel, _window, cx| {
                this.cancel_and_maybe_suppress(cx);
            }))
            .header(
                v_flex()
                    .p_3()
                    .gap_1()
                    .rounded_t_md()
                    .bg(cx.theme().colors().editor_background.opacity(0.5))
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Warning).color(Color::Warning))
                            .child(
                                Headline::new("Builds won't run on shared storage")
                                    .size(HeadlineSize::Small),
                            ),
                    )
                    .child(
                        h_flex()
                            .pl_5()
                            .child(Label::new(path_label).color(Color::Muted)),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .child(
                                Label::new(
                                    "This project lives on shared external storage, which is FUSE-mounted with `noexec`.",
                                )
                                .color(Color::Muted),
                            )
                            .child(
                                Label::new(
                                    "The kernel refuses to execve any file under it, so native build tools EACCES the moment they try to run a compiled binary.",
                                )
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        v_flex()
                            .child(Label::new("Affected:").color(Color::Muted))
                            .child(ListBulletItem::new("cargo / rustc target dir builds"))
                            .child(ListBulletItem::new("go build / make / autotools"))
                            .child(ListBulletItem::new("npm rebuild / native node modules"))
                            .child(ListBulletItem::new("Any pip install --no-binary native wheel")),
                    )
                    .child(
                        Checkbox::new(
                            "noexec-suppress",
                            ToggleState::from(self.suppress),
                        )
                        .label("Don't warn me about this project again")
                        .on_click(cx.listener(
                            |this, state: &ToggleState, _, cx| {
                                this.suppress = state.selected();
                                cx.notify();
                                cx.stop_propagation();
                            },
                        )),
                    ),
            )
            .footer(
                h_flex()
                    .px_3()
                    .pb_3()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new("noexec-cancel", "Cancel")
                            .key_binding(
                                KeyBinding::for_action(&menu::Cancel, cx)
                                    .size(rems_from_px(12.)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_and_maybe_suppress(cx);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        Button::new("noexec-copy", "Copy to ~/projects")
                            .style(ButtonStyle::Filled)
                            .layer(ElevationIndex::ModalSurface)
                            .key_binding(
                                KeyBinding::for_action(&menu::Confirm, cx)
                                    .size(rems_from_px(12.)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_and_dismiss(cx);
                                cx.stop_propagation();
                            })),
                    ),
            )
    }
}
