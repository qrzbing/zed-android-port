//! Android title bar — sits below `menu_bar` and shows the
//! Restricted Mode trust badge (when applicable), the project name,
//! and the settings/menu chevron whose tap toggles the menu bar
//! above and whose right-click / two-finger-tap opens the same
//! dropdown production puts in its application_menu (Settings,
//! Keymap, Themes, Icon Themes, Extensions).
//!
//! We render this inside the workspace's single `titlebar_item` slot
//! by stacking on top of the menu bar — see `header.rs`.

use gpui::{
    Action, Anchor, App, Context, DismissEvent, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    Render, StatefulInteractiveElement, Styled, WeakEntity, Window, anchored, deferred, div,
    prelude::FluentBuilder,
};
use project::trusted_worktrees::TrustedWorktrees;
use theme::ActiveTheme;
use ui::{
    Button, ButtonCommon, ButtonStyle, Clickable, Color, ContextMenu, Icon, IconButton, IconName,
    IconPosition, IconSize, Label, LabelCommon, LabelSize, TintColor, Tooltip, h_flex,
};
use workspace::Workspace;

use crate::menu_bar::MenuBar;

pub struct TitleBar {
    workspace: WeakEntity<Workspace>,
    menu_bar: WeakEntity<MenuBar>,
    /// When the chevron is right-clicked / two-finger-tapped, we
    /// build a ContextMenu, anchor it at the click position, and
    /// keep the entity alive in this slot until it dismisses. Same
    /// lifecycle pattern as `project_panel`'s `context_menu`. The
    /// `gpui::Subscription` field cannot be Clone'd, so we never
    /// `.cloned()` this whole tuple — only the menu+position pair
    /// when rendering, holding the subscription by reference.
    settings_menu: Option<(Entity<ContextMenu>, Point<Pixels>, gpui::Subscription)>,
    focus_handle: FocusHandle,
}

impl TitleBar {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        menu_bar: WeakEntity<MenuBar>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            workspace,
            menu_bar,
            settings_menu: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn render_restricted_mode(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let workspace = self.workspace.upgrade()?;
        let project = workspace.read(cx).project().clone();
        let has_restricted = TrustedWorktrees::try_get_global(cx)
            .map(|trusted| {
                trusted
                    .read(cx)
                    .has_restricted_worktrees(&project.read(cx).worktree_store(), cx)
            })
            .unwrap_or(false);
        if !has_restricted {
            return None;
        }

        let workspace_for_click = self.workspace.clone();
        Some(
            Button::new("zed-android-restricted-mode", "Restricted Mode")
                .style(ButtonStyle::Tinted(TintColor::Warning))
                .label_size(LabelSize::Small)
                .color(Color::Warning)
                .start_icon(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .tooltip(|_, cx| {
                    Tooltip::simple(
                        "You're in Restricted Mode — tap to trust this project",
                        cx,
                    )
                })
                .on_click(move |_, window, cx| {
                    let _ = workspace_for_click.update(cx, |workspace, cx| {
                        workspace.show_worktree_trust_security_modal(true, window, cx)
                    });
                })
                .into_any_element(),
        )
    }

    fn render_project_name(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let workspace = self.workspace.upgrade()?;
        let project = workspace.read(cx).project().read(cx);
        let mut visible = project.visible_worktrees(cx);
        let first = visible.next()?;
        // RelPath has no Display; production calls `.display(path_style)`
        // which returns a Cow<str>. We pass the worktree's own path style.
        let worktree = first.read(cx);
        let root = worktree.root_name().display(worktree.path_style()).to_string();
        Some(
            Label::new(root)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        )
    }

    fn render_chevron(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let menu_bar_for_toggle = self.menu_bar.clone();
        let toggle_click = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = menu_bar_for_toggle.update(cx, |bar, cx| {
                bar.hidden = !bar.hidden;
                cx.notify();
            });
        };

        let secondary = cx.listener(
            |this: &mut Self, event: &MouseDownEvent, window: &mut Window, cx| {
                this.deploy_settings_menu(event.position, window, cx);
                cx.stop_propagation();
            },
        );

        div()
            .id("zed-android-titlebar-chevron-wrap")
            .on_mouse_down(MouseButton::Right, secondary)
            .child(
                IconButton::new("zed-android-titlebar-chevron-btn", IconName::ChevronDown)
                    .icon_size(IconSize::Small)
                    .style(ButtonStyle::Subtle)
                    .tooltip(|_, cx| {
                        Tooltip::simple(
                            "Tap: toggle menu bar  ·  right-click / two-finger tap: settings",
                            cx,
                        )
                    })
                    .on_click(toggle_click),
            )
            .into_any_element()
    }

    fn deploy_settings_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.action("Settings", zed_actions::OpenSettings.boxed_clone())
                .action("Keymap", Box::new(zed_actions::OpenKeymap))
                .action(
                    "Themes…",
                    zed_actions::theme_selector::Toggle::default().boxed_clone(),
                )
                .action(
                    "Icon Themes…",
                    zed_actions::icon_theme_selector::Toggle::default().boxed_clone(),
                )
                .action(
                    "Extensions",
                    zed_actions::Extensions::default().boxed_clone(),
                )
        });
        let menu_focus = menu.focus_handle(cx);
        window.focus(&menu_focus, cx);
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.settings_menu.take();
            cx.notify();
        });
        self.settings_menu = Some((menu, position, subscription));
        cx.notify();
    }
}

impl Focusable for TitleBar {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let restricted = self.render_restricted_mode(cx);
        let project_name = self.render_project_name(cx);
        let chevron = self.render_chevron(cx);

        h_flex()
            .w_full()
            .h_6()
            .px_2()
            .gap_2()
            .items_center()
            .bg(cx.theme().colors().title_bar_background)
            .children(restricted)
            .children(project_name)
            .child(div().flex_1())
            .child(chevron)
            .when_some(
                self.settings_menu
                    .as_ref()
                    .map(|(menu, position, _)| (menu.clone(), *position)),
                |container, (menu, position)| {
                    container.child(
                        deferred(
                            anchored()
                                .position(position)
                                .anchor(Anchor::TopRight)
                                .child(menu),
                        )
                        .with_priority(1),
                    )
                },
            )
            .into_any_element()
    }
}
