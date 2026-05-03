//! Always-on application menu bar for Android.
//!
//! Production zed builds a `Vec<gpui::Menu>` via `app_menus()`, hands it to
//! `cx.set_menus(...)`, and lets the platform render it (macOS native menu
//! bar; Linux/Windows `title_bar` crate). Android has no native menu
//! surface and `title_bar` drags in `auto_update`/`call`/`livekit_client`/
//! `git_ui` we don't ship — so we render our own.
//!
//! The bar lives as the workspace's `titlebar_item` so it sits above the
//! tab bar / dock chrome, can be toggled hidden via the rightmost
//! affordance, and dispatches the same actions production's menu items
//! dispatch (so behaviour is identical to clicking `Open Project` from
//! macOS' File menu).
use std::sync::Mutex;

use gpui::{
    Action, App, Context, Entity, Focusable, IntoElement, Render, WeakEntity, Window, actions,
};
use ui::prelude::*;
use ui::{ContextMenu, PopoverMenu, PopoverMenuHandle, Tooltip};
use workspace::Workspace;

actions!(
    zed_android,
    [
        /// Toggles the always-on Android application menu bar.
        ToggleAppMenuBar
    ]
);

static ACTIVE_MENU_BAR: Mutex<Option<WeakEntity<MenuBar>>> = Mutex::new(None);

pub fn register_actions(cx: &mut App) {
    cx.on_action(|_: &ToggleAppMenuBar, cx: &mut App| {
        let weak = ACTIVE_MENU_BAR.lock().ok().and_then(|g| g.clone());
        if let Some(weak) = weak {
            weak.update(cx, |bar, cx| {
                bar.hidden = !bar.hidden;
                cx.notify();
            })
            .ok();
        }
    });
    cx.bind_keys([gpui::KeyBinding::new(
        "ctrl-alt-m",
        ToggleAppMenuBar,
        None,
    )]);
}

pub struct MenuBar {
    pub hidden: bool,
    workspace: WeakEntity<Workspace>,
    menus: Vec<MenuDefinition>,
    handles: Vec<PopoverMenuHandle<ContextMenu>>,
}

pub fn register(entity: &Entity<MenuBar>) {
    if let Ok(mut slot) = ACTIVE_MENU_BAR.lock() {
        *slot = Some(entity.downgrade());
    }
}

struct MenuDefinition {
    title: &'static str,
    items: fn() -> Vec<MenuEntry>,
}

enum MenuEntry {
    Action(&'static str, Box<dyn Action>),
    Separator,
}

impl MenuBar {
    pub fn new(workspace: WeakEntity<Workspace>, _cx: &mut Context<Self>) -> Self {
        let menus = app_menu_definitions();
        let handles = menus
            .iter()
            .map(|_| PopoverMenuHandle::default())
            .collect();
        Self {
            hidden: false,
            workspace,
            menus,
            handles,
        }
    }
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.hidden {
            // Render nothing — the workspace's titlebar slot collapses
            // and the editor / dock chrome occupies the freed pixels.
            // Restore via the `ToggleAppMenuBar` action (Ctrl+Alt+M, or
            // dispatch from the command palette).
            return div().w_0().h_0().into_any_element();
        }

        let mut buttons: Vec<gpui::AnyElement> = Vec::with_capacity(self.menus.len());
        for (idx, menu) in self.menus.iter().enumerate() {
            let title = menu.title;
            let items = menu.items;
            let handle = self.handles[idx].clone();
            // The action context is the workspace's focus handle: when the
            // menu fires `Save` / `Cut` / etc., it focuses this handle and
            // dispatches there so workspace's `register_action(...)` and
            // the active editor's element-level handlers actually run.
            // Without this, dispatch goes through whatever stray focus
            // remains after the popover dismisses and the action
            // silently misses its target.
            let workspace = self.workspace.clone();
            buttons.push(
                PopoverMenu::new(("zed-android-menubar", idx))
                    .menu({
                        let workspace = workspace.clone();
                        move |window, cx| {
                            // Snapshot of the active pane's item right now —
                            // this gives us the editor's focus node (Pane
                            // forwards focus to its active item). We
                            // dispatch via this handle directly rather than
                            // relying on `window.dispatch_action`, which
                            // looks up the *currently* focused element in
                            // the rendered frame. After the popover takes
                            // focus and dismisses, that lookup misses the
                            // editor and editor::Cut / Copy / Paste / etc.
                            // never reach a handler.
                            let active_focus = workspace
                                .read_with(cx, |workspace, cx| {
                                    let pane = workspace.active_pane().read(cx);
                                    pane.active_item()
                                        .map(|item| item.item_focus_handle(cx))
                                        .unwrap_or_else(|| pane.focus_handle(cx))
                                })
                                .ok();
                            Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                if let Some(ref handle) = active_focus {
                                    menu = menu.context(handle.clone());
                                }
                                for entry in items() {
                                    match entry {
                                        MenuEntry::Action(label, action) => {
                                            let dispatch_via = active_focus.clone();
                                            let action_for_handler = action.boxed_clone();
                                            menu = menu.entry(
                                                label,
                                                Some(action),
                                                move |window, cx| {
                                                    let action = action_for_handler.boxed_clone();
                                                    if let Some(ref handle) = dispatch_via {
                                                        handle.dispatch_action(
                                                            action.as_ref(),
                                                            window,
                                                            cx,
                                                        );
                                                    } else {
                                                        window.dispatch_action(action, cx);
                                                    }
                                                },
                                            );
                                        }
                                        MenuEntry::Separator => {
                                            menu = menu.separator();
                                        }
                                    }
                                }
                                menu
                            }))
                        }
                    })
                    .with_handle(handle)
                    .trigger(
                        Button::new(("zed-android-menubar-trigger", idx), title)
                            .label_size(LabelSize::Small)
                            .style(ButtonStyle::Subtle),
                    )
                    .into_any_element(),
            );
        }

        h_flex()
            .w_full()
            .h_6()
            .px_2()
            .gap_3()
            .items_center()
            .bg(cx.theme().colors().title_bar_background)
            .children(buttons)
            .child(div().flex_1())
            .child(
                IconButton::new("zed-android-menubar-hide", IconName::ChevronUp)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::for_action_title(
                        "Hide Application Menu (Ctrl+Alt+M to restore)",
                        &ToggleAppMenuBar,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hidden = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

/// Mirrors production's `app_menus()` minus entries whose actions live
/// in crates we don't yet ship (terminal_view, debugger, collab_panel,
/// recent_projects, etc.). Add them here as those crates land in the
/// example's deps; the rest stays unchanged.
fn app_menu_definitions() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition {
            title: "Zed",
            items: || {
                vec![
                    MenuEntry::Action("Open Settings", Box::new(zed_actions::OpenSettings)),
                    MenuEntry::Action("Open Keymap", Box::new(zed_actions::OpenKeymap)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Welcome",
                        Box::new(workspace::welcome::ShowWelcome),
                    ),
                    MenuEntry::Action(
                        "Onboarding",
                        Box::new(zed_actions::OpenOnboarding),
                    ),
                    MenuEntry::Separator,
                    MenuEntry::Action("Quit", Box::new(zed_actions::Quit)),
                ]
            },
        },
        MenuDefinition {
            title: "File",
            items: || {
                vec![
                    MenuEntry::Action("New File", Box::new(workspace::NewFile)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Open…",
                        Box::new(workspace::Open::default()),
                    ),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Save",
                        Box::new(workspace::Save { save_intent: None }),
                    ),
                    MenuEntry::Action("Save As…", Box::new(workspace::SaveAs)),
                    MenuEntry::Action(
                        "Save All",
                        Box::new(workspace::SaveAll { save_intent: None }),
                    ),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Close Editor",
                        Box::new(workspace::CloseActiveItem::default()),
                    ),
                ]
            },
        },
        MenuDefinition {
            title: "Edit",
            items: || {
                vec![
                    MenuEntry::Action("Undo", Box::new(editor::actions::Undo)),
                    MenuEntry::Action("Redo", Box::new(editor::actions::Redo)),
                    MenuEntry::Separator,
                    MenuEntry::Action("Cut", Box::new(editor::actions::Cut)),
                    MenuEntry::Action("Copy", Box::new(editor::actions::Copy)),
                    MenuEntry::Action("Paste", Box::new(editor::actions::Paste)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Find in Buffer…",
                        Box::new(zed_actions::buffer_search::Deploy::find()),
                    ),
                    MenuEntry::Action(
                        "Find in Project…",
                        Box::new(workspace::DeploySearch::default()),
                    ),
                ]
            },
        },
        MenuDefinition {
            title: "Selection",
            items: || {
                vec![
                    MenuEntry::Action("Select All", Box::new(editor::actions::SelectAll)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Add Cursor Above",
                        Box::new(editor::actions::AddSelectionAbove::default()),
                    ),
                    MenuEntry::Action(
                        "Add Cursor Below",
                        Box::new(editor::actions::AddSelectionBelow::default()),
                    ),
                    MenuEntry::Action("Select Line", Box::new(editor::actions::SelectLine)),
                ]
            },
        },
        MenuDefinition {
            title: "View",
            items: || {
                vec![
                    MenuEntry::Action("Toggle Left Dock", Box::new(workspace::ToggleLeftDock)),
                    MenuEntry::Action("Toggle Right Dock", Box::new(workspace::ToggleRightDock)),
                    MenuEntry::Action(
                        "Toggle Bottom Dock",
                        Box::new(workspace::ToggleBottomDock),
                    ),
                    MenuEntry::Action("Toggle All Docks", Box::new(workspace::ToggleAllDocks)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Project Panel",
                        Box::new(zed_actions::project_panel::ToggleFocus),
                    ),
                    MenuEntry::Action(
                        "Outline Panel",
                        Box::new(outline_panel::ToggleFocus),
                    ),
                    MenuEntry::Separator,
                    MenuEntry::Action("Diagnostics", Box::new(diagnostics::Deploy)),
                ]
            },
        },
        MenuDefinition {
            title: "Go",
            items: || {
                vec![
                    MenuEntry::Action("Back", Box::new(workspace::GoBack)),
                    MenuEntry::Action("Forward", Box::new(workspace::GoForward)),
                    MenuEntry::Separator,
                    MenuEntry::Action(
                        "Command Palette…",
                        Box::new(zed_actions::command_palette::Toggle),
                    ),
                ]
            },
        },
    ]
}
