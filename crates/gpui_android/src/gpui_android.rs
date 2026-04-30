#![cfg(target_os = "android")]
#![doc = "Android backend for the GPUI Platform trait. Phase 2: stub. Real implementations land in subsequent phases."]

mod dispatcher;

pub(crate) use dispatcher::AndroidDispatcher;

use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, ForegroundExecutor,
    Keymap, Menu, MenuItem, PathPromptOptions, Platform, PlatformDisplay, PlatformKeyboardLayout,
    PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, Task, ThermalState,
    WindowAppearance, WindowParams,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

pub struct AndroidPlatform;

impl AndroidPlatform {
    pub fn new(_headless: bool) -> Self {
        Self
    }
}

impl Platform for AndroidPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        unimplemented!("AndroidPlatform::background_executor — Phase 3 dispatcher")
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        unimplemented!("AndroidPlatform::foreground_executor — Phase 3 dispatcher")
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        unimplemented!("AndroidPlatform::text_system — Phase 5 (cosmic-text via gpui_wgpu)")
    }

    fn run(&self, _on_finish_launching: Box<dyn 'static + FnOnce()>) {
        unimplemented!("AndroidPlatform::run — Phase 3 (android-activity ALooper)")
    }

    fn quit(&self) {}

    fn restart(&self, _binary_path: Option<PathBuf>) {}

    fn activate(&self, _ignoring_other_apps: bool) {}

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        Vec::new()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        None
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        None
    }

    fn open_window(
        &self,
        _handle: AnyWindowHandle,
        _options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        unimplemented!("AndroidPlatform::open_window — Phase 3/4 (ANativeWindow + wgpu surface)")
    }

    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }

    fn open_url(&self, _url: &str) {}

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {}

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!(
            "prompt_for_paths is not yet implemented on Android"
        )))
        .ok();
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!(
            "prompt_for_new_path is not yet implemented on Android"
        )))
        .ok();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {}

    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, _callback: Box<dyn FnMut()>) {}

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}

    fn compositor_name(&self) -> &'static str {
        "Android"
    }

    fn app_path(&self) -> Result<PathBuf> {
        Err(anyhow::anyhow!("app_path is not yet implemented on Android"))
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        Err(anyhow::anyhow!(
            "path_for_auxiliary_executable is not available on Android"
        ))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        None
    }

    fn write_to_clipboard(&self, _item: ClipboardItem) {}

    fn write_credentials(
        &self,
        _url: &str,
        _username: &str,
        _password: &[u8],
    ) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "credential storage is not yet implemented on Android"
        )))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "credential storage is not yet implemented on Android"
        )))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        unimplemented!("AndroidPlatform::keyboard_layout — Phase 6")
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        unimplemented!("AndroidPlatform::keyboard_mapper — Phase 6")
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}
}
