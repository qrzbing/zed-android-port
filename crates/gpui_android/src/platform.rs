use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use android_activity::AndroidApp;
use ndk::configuration::UiModeNight;
use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DummyKeyboardMapper,
    ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform, PlatformDisplay,
    PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow,
    PriorityQueueReceiver, RunnableVariant, Task, ThermalState, WindowAppearance, WindowParams,
};
use gpui_wgpu::GpuContext;

use crate::dispatcher::AndroidDispatcher;
use crate::display::AndroidDisplay;
use crate::keyboard::AndroidKeyboardLayout;
use crate::window::{AndroidWindow, AndroidWindowStatePtr};

#[derive(Default)]
pub(crate) struct PlatformHandlers {
    pub(crate) open_urls: Option<Box<dyn FnMut(Vec<String>)>>,
    pub(crate) quit: Option<Box<dyn FnMut()>>,
    pub(crate) reopen: Option<Box<dyn FnMut()>>,
    pub(crate) app_menu_action: Option<Box<dyn FnMut(&dyn Action)>>,
    pub(crate) will_open_app_menu: Option<Box<dyn FnMut()>>,
    pub(crate) validate_app_menu_command: Option<Box<dyn FnMut(&dyn Action) -> bool>>,
    pub(crate) keyboard_layout_change: Option<Box<dyn FnMut()>>,
}

pub(crate) struct AndroidCommon {
    pub(crate) background_executor: BackgroundExecutor,
    pub(crate) foreground_executor: ForegroundExecutor,
    pub(crate) text_system: Arc<dyn PlatformTextSystem>,
    pub(crate) appearance: WindowAppearance,
    pub(crate) callbacks: PlatformHandlers,
    pub(crate) main_receiver: PriorityQueueReceiver<RunnableVariant>,
    pub(crate) active_window: Option<AnyWindowHandle>,
    pub(crate) gpu_context: GpuContext,
    /// The single live AndroidWindow. Android only supports one window per
    /// activity for our purposes; lifecycle events from the run loop are
    /// dispatched to whatever's stored here.
    pub(crate) window: Option<AndroidWindowStatePtr>,
    pub(crate) running: bool,
}

impl AndroidCommon {
    pub fn new(android_app: &AndroidApp) -> Self {
        let (dispatcher, main_receiver) = AndroidDispatcher::new(android_app);
        let dispatcher = Arc::new(dispatcher);

        let text_system: Arc<dyn PlatformTextSystem> = Arc::new(
            gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Lilex"),
        );

        Self {
            background_executor: BackgroundExecutor::new(dispatcher.clone()),
            foreground_executor: ForegroundExecutor::new(dispatcher),
            text_system,
            appearance: appearance_from_config(android_app),
            callbacks: PlatformHandlers::default(),
            main_receiver,
            active_window: None,
            gpu_context: Rc::new(RefCell::new(None)),
            window: None,
            running: true,
        }
    }
}

fn appearance_from_config(android_app: &AndroidApp) -> WindowAppearance {
    match android_app.config().ui_mode_night() {
        UiModeNight::Yes => WindowAppearance::Dark,
        _ => WindowAppearance::Light,
    }
}

pub struct AndroidPlatform {
    pub(crate) common: RefCell<AndroidCommon>,
    pub(crate) android_app: AndroidApp,
}

impl AndroidPlatform {
    pub fn new(android_app: AndroidApp, _headless: bool) -> Self {
        let common = AndroidCommon::new(&android_app);
        Self {
            common: RefCell::new(common),
            android_app,
        }
    }

    /// Read the device's display density and convert to a scale factor.
    /// Android reports density in dpi where 160 dpi = 1.0x. Tab S9 Ultra
    /// reports ~336 dpi (~2.1x). Falls back to 1.0 if the density isn't yet
    /// reported (e.g. very early in startup before the first config arrives).
    fn compute_scale_factor(&self) -> f32 {
        match self.android_app.config().density() {
            Some(dpi) if dpi > 0 => (dpi as f32 / 160.0).max(1.0),
            _ => 1.0,
        }
    }

    /// Pull every queued `InputEvent` off android-activity's iterator and
    /// route translatable ones into the active gpui window. Returning
    /// `InputStatus::Handled` for our own events lets android-activity stop
    /// propagating them up the system input stack (e.g. so a keyboard ENTER
    /// doesn't also dismiss the keyboard).
    fn drain_input_events(&self) {
        use android_activity::input::InputEvent;
        use android_activity::InputStatus;

        let Some(window_ptr) = self.common.borrow().window.clone() else {
            return;
        };
        let Ok(mut iter) = self.android_app.input_events_iter() else {
            return;
        };
        let scale_factor = self.compute_scale_factor();
        loop {
            let read = iter.next(|event| match event {
                InputEvent::KeyEvent(key) => match crate::events::translate_key_event(key) {
                    Some(input) => {
                        window_ptr.handle_input(input);
                        InputStatus::Handled
                    }
                    None => InputStatus::Unhandled,
                },
                InputEvent::MotionEvent(motion) => {
                    let inputs = crate::events::translate_motion_event(motion, scale_factor);
                    if inputs.is_empty() {
                        InputStatus::Unhandled
                    } else {
                        for input in inputs {
                            window_ptr.handle_input(input);
                        }
                        InputStatus::Handled
                    }
                }
                _ => InputStatus::Unhandled,
            });
            if !read {
                break;
            }
        }
    }

    fn handle_main_event(&self, event: android_activity::MainEvent<'_>) {
        use android_activity::MainEvent;
        match event {
            MainEvent::InitWindow { .. } => {
                let window_ptr = self.common.borrow().window.clone();
                let Some(window_ptr) = window_ptr else {
                    log::warn!("MainEvent::InitWindow received before any window registered");
                    return;
                };
                let Some(native_window) = self.android_app.native_window() else {
                    log::warn!("MainEvent::InitWindow but native_window() returned None");
                    return;
                };
                let scale_factor = self.compute_scale_factor();
                if let Err(e) = window_ptr.attach_surface(native_window, scale_factor) {
                    log::error!("attach_surface failed: {e:#}");
                }
            }
            MainEvent::TerminateWindow { .. } => {
                if let Some(window_ptr) = self.common.borrow().window.clone() {
                    window_ptr.detach_surface();
                }
            }
            MainEvent::WindowResized { .. } => {
                let window_ptr = self.common.borrow().window.clone();
                let Some(window_ptr) = window_ptr else { return };
                let Some(native_window) = self.android_app.native_window() else { return };
                window_ptr.resize_surface(
                    native_window.width() as u32,
                    native_window.height() as u32,
                    self.compute_scale_factor(),
                );
            }
            MainEvent::ConfigChanged { .. } => {
                // Density may have changed (rotation, dock/scaling). Refresh
                // scale_factor and re-emit a resize so layout picks it up.
                let window_ptr = self.common.borrow().window.clone();
                let Some(window_ptr) = window_ptr else { return };
                let Some(native_window) = self.android_app.native_window() else { return };
                window_ptr.resize_surface(
                    native_window.width() as u32,
                    native_window.height() as u32,
                    self.compute_scale_factor(),
                );
                let new_appearance = appearance_from_config(&self.android_app);
                self.common.borrow_mut().appearance = new_appearance;
                window_ptr.set_appearance(new_appearance);
            }
            MainEvent::RedrawNeeded { .. } => {
                if let Some(window_ptr) = self.common.borrow().window.clone() {
                    window_ptr.refresh();
                }
            }
            MainEvent::Destroy => {
                self.common.borrow_mut().running = false;
            }
            _ => {}
        }
    }
}

impl Platform for AndroidPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.common.borrow().background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.common.borrow().foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.common.borrow().text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        log::info!("AndroidPlatform::run: invoking on_finish_launching");
        on_finish_launching();
        log::info!("AndroidPlatform::run: entering event loop");

        while self.common.borrow().running {
            // Block until: timeout, waker, or a main-event from android-activity.
            self.android_app.poll_events(
                Some(std::time::Duration::from_millis(16)),
                |event| match event {
                    android_activity::PollEvent::Wake => {}
                    android_activity::PollEvent::Timeout => {}
                    android_activity::PollEvent::Main(main_event) => {
                        log::trace!("MainEvent: {main_event:?}");
                        self.handle_main_event(main_event);
                    }
                    _ => {}
                },
            );

            // Drain main-thread runnables enqueued from background threads. The
            // AndroidAppWaker wakes poll_events above when there's work; we drain
            // each tick regardless to catch anything between waker and poll.
            let receiver = self.common.borrow().main_receiver.clone();
            for runnable in receiver.try_iter() {
                if let Ok(runnable) = runnable {
                    runnable.run();
                }
            }

            // Drain input events into the active window. We call this every
            // tick rather than only on Wake — android-activity gates input
            // delivery on this iterator being polled, so missing a tick can
            // stall touch.
            self.drain_input_events();

            // Drive a paint at the poll cadence (~60Hz from the 16ms timeout).
            // gpui's request_frame callback short-circuits when nothing has
            // changed, so this is cheap when idle. A future pass should hook
            // Android's Choreographer for proper vsync alignment.
            if let Some(window_ptr) = self.common.borrow().window.clone() {
                window_ptr.refresh();
            }
        }

        log::info!("AndroidPlatform::run: exiting event loop");
        let quit = self.common.borrow_mut().callbacks.quit.take();
        if let Some(mut fun) = quit {
            fun();
        }
    }

    fn quit(&self) {
        self.common.borrow_mut().running = false;
        let quit = self.common.borrow_mut().callbacks.quit.take();
        if let Some(mut fun) = quit {
            fun();
        }
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {}
    fn activate(&self, _ignoring_other_apps: bool) {}
    fn hide(&self) {}
    fn hide_other_apps(&self) {}
    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![Rc::new(AndroidDisplay::new())]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(AndroidDisplay::new()))
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.common.borrow().active_window
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        // Android only supports one window. The second `cx.open_window` call
        // (e.g. `workspace::open_paths` falling through to `Workspace::new_local`
        // when there's no existing target) tries to create another VkSurfaceKHR
        // for the same `ANativeWindow` and panics with
        // `ERROR_NATIVE_WINDOW_IN_USE_KHR`. Fail cleanly so the caller's
        // `.log_err()` reports it instead of taking the process down.
        if self.common.borrow().window.is_some() {
            return Err(anyhow::anyhow!(
                "open_window: gpui_android only supports a single window; \
                 callers should reuse the active window via `requesting_window` \
                 (and `add_dirs_to_sidebar` / `should_reuse_existing_window`)"
            ));
        }

        // gpui's `Window::new` calls `platform_window.sprite_atlas()` immediately,
        // so the renderer (and therefore atlas) must already be live by the time
        // we return. On Android we have no surface until `MainEvent::InitWindow`
        // fires, so block here pumping the Android event loop until a native
        // window is available, then attach inline.
        //
        // poll_events is reentrant-safe — `on_finish_launching` runs before our
        // outer poll loop in `run()`, so this is the only `poll_events` call
        // on the stack right now.
        while self.android_app.native_window().is_none() {
            if !self.common.borrow().running {
                return Err(anyhow::anyhow!(
                    "open_window: app destroyed before surface attached"
                ));
            }
            self.android_app.poll_events(
                Some(std::time::Duration::from_millis(100)),
                |event| {
                    if let android_activity::PollEvent::Main(main_event) = event {
                        log::trace!("MainEvent during open_window block: {main_event:?}");
                        self.handle_main_event(main_event);
                    }
                },
            );

            // NOTE: do not drain main_receiver here. open_window runs inside
            // gpui's `cx.update` borrow guard; a runnable that calls
            // `cx.update(...)` re-enters the borrow and panics ("RefCell
            // already borrowed"). The outer event loop drains the queue on
            // the next tick, so queued work picks up fine.
        }

        let appearance = self.common.borrow().appearance;
        let gpu_context = self.common.borrow().gpu_context.clone();
        let window = AndroidWindow::new(handle, options, gpu_context, appearance);

        let native_window = self.android_app.native_window().ok_or_else(|| {
            anyhow::anyhow!("open_window: native_window vanished between poll and attach")
        })?;
        let scale_factor = self.compute_scale_factor();
        window.ptr().attach_surface(native_window, scale_factor)?;

        self.common.borrow_mut().window = Some(window.ptr());
        self.common.borrow_mut().active_window = Some(handle);
        Ok(Box::new(window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.common.borrow().appearance
    }

    fn open_url(&self, _url: &str) {}
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.common.borrow_mut().callbacks.open_urls = Some(callback);
    }
    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        // Fire ACTION_OPEN_DOCUMENT_TREE via MainActivity. The result
        // arrives async through the JNI callback in `saf.rs`.
        log::info!("AndroidPlatform::prompt_for_paths invoked");
        let (tx, rx) = oneshot::channel();
        crate::saf::pick_folder(&self.android_app, tx);
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        // ACTION_CREATE_DOCUMENT — the system picker decides the
        // directory; we only suggest a name. Result arrives via the
        // same JNI callback.
        log::info!(
            "AndroidPlatform::prompt_for_new_path invoked (suggested={:?})",
            suggested_name
        );
        let (tx, rx) = oneshot::channel();
        crate::saf::pick_new_path(&self.android_app, suggested_name, tx);
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }
    fn reveal_path(&self, _path: &Path) {}
    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, callback: Box<dyn FnMut()>) {
        self.common.borrow_mut().callbacks.quit = Some(callback);
    }
    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.common.borrow_mut().callbacks.reopen = Some(callback);
    }
    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}
    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.common.borrow_mut().callbacks.app_menu_action = Some(callback);
    }
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.common.borrow_mut().callbacks.will_open_app_menu = Some(callback);
    }
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.common.borrow_mut().callbacks.validate_app_menu_command = Some(callback);
    }

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
            "auxiliary executables are not available on Android"
        ))
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        crate::cursor::set_pointer_icon(&self.android_app, style);
    }
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
            "credential storage not yet wired on Android"
        )))
    }
    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }
    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "credential storage not yet wired on Android"
        )))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(AndroidKeyboardLayout)
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.common.borrow_mut().callbacks.keyboard_layout_change = Some(callback);
    }
}
