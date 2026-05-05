use std::{
    cell::RefCell,
    ffi::c_void,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};

use android_activity::AndroidApp;
use ndk::configuration::UiModeNight;
use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, BackgroundExecutor, Bounds, ClipboardItem, CursorStyle,
    DummyKeyboardMapper, ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Pixels,
    Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, RunnableVariant, Task, ThermalState, WindowAppearance,
    WindowParams,
};
use gpui_wgpu::GpuContext;

use crate::dispatcher::AndroidDispatcher;
use crate::display::AndroidDisplay;
use crate::keyboard::AndroidKeyboardLayout;
use crate::window::{AndroidWindow, AndroidWindowStatePtr};

/// AChoreographer FFI — NDK API 24+. We avoid going through Java's
/// android.view.Choreographer because we'd need a JNI hop on every
/// vsync; the NDK exposes the same scheduler natively. Linked from
/// libandroid.so which Android already keeps mapped.
#[link(name = "android")]
unsafe extern "C" {
    fn AChoreographer_getInstance() -> *mut c_void;
    fn AChoreographer_postFrameCallback64(
        choreographer: *mut c_void,
        callback: ChoreographerFrameCallback,
        data: *mut c_void,
    );
}

type ChoreographerFrameCallback =
    unsafe extern "C" fn(frame_time_nanos: i64, data: *mut c_void);

/// Set by the Choreographer callback (called once per vsync on the
/// main thread's looper, so no synchronization beyond the atomic).
/// Drained at the top of each event-loop tick to decide whether to
/// call window.refresh().
static FRAME_PENDING: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn frame_callback(_frame_time_nanos: i64, _data: *mut c_void) {
    FRAME_PENDING.store(true, Ordering::Release);
    // Re-post for the next vsync so we get a continuous stream of
    // frame callbacks. Stopping this stream means the next tick won't
    // get a vsync wake-up; we always want to be ready to render.
    unsafe {
        let c = AChoreographer_getInstance();
        if !c.is_null() {
            AChoreographer_postFrameCallback64(
                c,
                frame_callback,
                std::ptr::null_mut(),
            );
        }
    }
}

/// Register the first Choreographer frame callback on the calling
/// thread (must have a Looper attached — android-activity's main
/// thread does). Subsequent callbacks self-re-post inside
/// `frame_callback` so we keep getting vsync ticks for the app's
/// lifetime.
fn install_choreographer_callback() {
    unsafe {
        let c = AChoreographer_getInstance();
        if c.is_null() {
            log::warn!(
                "AChoreographer_getInstance returned null; vsync sync \
                 disabled, falling back to poll-timeout-driven refresh"
            );
            return;
        }
        AChoreographer_postFrameCallback64(c, frame_callback, std::ptr::null_mut());
    }
    log::info!("AndroidPlatform: Choreographer frame callback registered");
}

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
    /// The GameActivity-owned primary window. First `cx.open_window` lands
    /// here; subsequent calls go to [`extra_windows`].
    pub(crate) window: Option<AndroidWindowStatePtr>,
    /// Secondary `cx.open_window` results, keyed by `WindowId::as_u64()`.
    /// Each is hosted in its own `ExtraWindowActivity`; the OS owns the
    /// chrome and routes touch/keyboard input straight to that Activity's
    /// `SurfaceView`. We just need the state ptr to dispatch resize and
    /// motion events delivered through the `extra_event_rx` channel.
    pub(crate) extra_windows: std::collections::HashMap<u64, AndroidWindowStatePtr>,
    /// Receiver side of the JNI → game-thread channel for extra-window
    /// events. Drained each iteration of the platform run loop. `Some`
    /// from `AndroidCommon::new` until the loop terminates.
    pub(crate) extra_event_rx: Option<futures::channel::mpsc::UnboundedReceiver<crate::multi_window::ExtraWindowEvent>>,
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
            extra_windows: std::collections::HashMap::new(),
            extra_event_rx: Some(crate::multi_window::init_event_channel()),
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

    /// Translate the gpui-side `WindowParams.bounds` (in logical pixels at
    /// the primary's scale factor) into device-pixel `Rect` coordinates the
    /// OS will use as the freeform window's initial size and position. Falls
    /// back to `None` (let the OS pick) if either the requested size is
    /// nonsense or we can't read screen dimensions.
    fn compute_launch_bounds(
        &self,
        bounds: &Bounds<Pixels>,
        scale_factor: f32,
    ) -> Option<crate::multi_window::LaunchBounds> {
        let width_px = (bounds.size.width.as_f32() * scale_factor).round() as i32;
        let height_px = (bounds.size.height.as_f32() * scale_factor).round() as i32;
        if width_px <= 0 || height_px <= 0 {
            return None;
        }
        let nw = self.android_app.native_window()?;
        let screen_w = nw.width() as i32;
        let screen_h = nw.height() as i32;
        if screen_w <= 0 || screen_h <= 0 {
            return None;
        }
        // Center the window on screen by default. Caller-supplied origin is
        // ignored for now — gpui's WindowParams.bounds.origin is meaningless
        // on Android (no window manager coordinate space prior to L7e).
        let left = ((screen_w - width_px) / 2).max(0);
        let top = ((screen_h - height_px) / 2).max(0);
        Some(crate::multi_window::LaunchBounds {
            left,
            top,
            right: left + width_px,
            bottom: top + height_px,
        })
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

    /// Branch of [`open_window`](Self::open_window) for the second-and-beyond
    /// `cx.open_window` call. Launches an `ExtraWindowActivity` via JNI Intent,
    /// waits up to 500ms for its `surfaceCreated` callback, then wraps the
    /// resulting `ANativeWindow` in a new `AndroidWindow`. The OS owns the
    /// chrome and routes touch/lifecycle events to that Activity's
    /// `SurfaceView`; events flow through `drain_extra_window_events`.
    ///
    /// `options.bounds` is informational only — actual placement is
    /// controlled by the OS window manager (or by ActivityOptions launch
    /// bounds, deferred to L7e).
    fn open_extra_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        let scale_factor = self.compute_scale_factor();
        let window_id = handle.window_id().as_u64();
        let launch_bounds = self.compute_launch_bounds(&options.bounds, scale_factor);
        log::info!(
            "open_extra_window: windowId={window_id} launching ExtraWindowActivity bounds={launch_bounds:?}"
        );

        // Mark the window as known BEFORE launching the Activity. If we
        // marked it later (e.g. after `attach_surface`), the Activity's
        // `onCreate` would race ahead and call `nativeIsExtraWindowKnown`
        // before the mark — getting a false negative and finishing itself
        // prematurely. The `unmark_window_registered` calls below cover the
        // failure paths.
        crate::multi_window::mark_window_registered(window_id);

        let native_window = match crate::multi_window::create_extra_window_blocking(
            &self.android_app,
            window_id,
            launch_bounds,
        ) {
            Ok(nw) => nw,
            Err(err) => {
                crate::multi_window::unmark_window_registered(window_id);
                return Err(err);
            }
        };

        let appearance = self.common.borrow().appearance;
        let gpu_context = self.common.borrow().gpu_context.clone();
        let mut window =
            AndroidWindow::new(handle, options, gpu_context, appearance, self.android_app.clone());
        window.extra_window_id = Some(window_id);
        if let Err(err) = window.ptr().attach_surface(native_window, scale_factor) {
            crate::multi_window::unmark_window_registered(window_id);
            return Err(err);
        }

        {
            let mut common = self.common.borrow_mut();
            common.extra_windows.insert(window_id, window.ptr());
            common.active_window = Some(handle);
        }
        // Fire the activation observers so the editor's
        // `cx.observe_window_activation` callback runs and enables cursor
        // blink (and any other activation-gated state). Our `is_active()`
        // returns true at construction, but gpui's observer machinery only
        // fires when the platform calls the active-status-change callback;
        // without this, the search field's cursor renders statically until
        // the user's first input. See `blink_manager.rs` and `editor.rs`'s
        // `cx.observe_window_activation` registration.
        //
        // Must defer to a later tick: gpui registers the
        // `on_active_status_change` callback inside `Window::new`, which
        // runs AFTER our `open_extra_window` returns. Firing synchronously
        // here would no-op against an empty callback slot.
        let executor = self.common.borrow().foreground_executor.clone();
        let window_ptr = window.ptr();
        executor
            .spawn(async move {
                window_ptr.notify_active_status_change(true);
            })
            .detach();
        Ok(Box::new(window))
    }

    /// Pull JNI-originated extra-window events off the channel and dispatch
    /// to the matching `AndroidWindowStatePtr`. Called once per iteration of
    /// the platform's main loop.
    fn drain_extra_window_events(&self) {
        use crate::multi_window::ExtraWindowEvent;

        let mut rx = match self.common.borrow_mut().extra_event_rx.take() {
            Some(rx) => rx,
            None => return,
        };

        while let Ok(event) = rx.try_recv() {
            match event {
                ExtraWindowEvent::Resized {
                    window_id,
                    width,
                    height,
                } => {
                    let entry = self.common.borrow().extra_windows.get(&window_id).cloned();
                    if let Some(state) = entry {
                        let scale = state.state.borrow().scale_factor;
                        state.resize_surface(width, height, scale);
                    } else {
                        log::warn!(
                            "drain_extra_window_events: Resized for unknown windowId={window_id}"
                        );
                    }
                }
                ExtraWindowEvent::SurfaceDestroyed { window_id } => {
                    let entry = self.common.borrow().extra_windows.get(&window_id).cloned();
                    if let Some(state) = entry {
                        state.detach_surface();
                    }
                }
                ExtraWindowEvent::OsClosed { window_id } => {
                    // OS-initiated close (user clicked chrome X). Set the
                    // os_closed flag so AndroidWindow::Drop will skip its
                    // JNI finishActivity call (Activity is already gone),
                    // then fire the gpui-registered `on_close` callback.
                    // gpui's callback drives `Window::remove_window()`, which
                    // in turn drops our `Box<dyn PlatformWindow>` and
                    // ultimately reaps the `extra_windows` map entry.
                    let entry = self.common.borrow().extra_windows.get(&window_id).cloned();
                    let Some(state) = entry else {
                        log::info!(
                            "drain_extra_window_events: OsClosed for already-removed windowId={window_id}"
                        );
                        crate::multi_window::unmark_window_registered(window_id);
                        continue;
                    };
                    state.state.borrow().os_closed.store(true, std::sync::atomic::Ordering::SeqCst);
                    let close_cb = state.callbacks.borrow_mut().close.take();
                    if let Some(cb) = close_cb {
                        log::info!(
                            "drain_extra_window_events: OsClosed windowId={window_id} → invoking gpui on_close"
                        );
                        cb();
                    } else {
                        // No callback registered — gpui never wired one. Tear
                        // down the platform-side state directly so we don't
                        // leak.
                        log::warn!(
                            "drain_extra_window_events: OsClosed windowId={window_id} but no on_close registered"
                        );
                        let mut common = self.common.borrow_mut();
                        common.extra_windows.remove(&window_id);
                        if common
                            .active_window
                            .as_ref()
                            .is_some_and(|h| h.window_id().as_u64() == window_id)
                        {
                            common.active_window = None;
                        }
                    }
                    crate::multi_window::unmark_window_registered(window_id);
                }
                ExtraWindowEvent::Motion {
                    window_id,
                    action_masked,
                    action_index,
                    meta_state,
                    button_state,
                    event_time_millis: _,
                    vscroll,
                    hscroll,
                    positions,
                } => {
                    let entry = self.common.borrow().extra_windows.get(&window_id).cloned();
                    let Some(state) = entry else {
                        log::warn!(
                            "drain_extra_window_events: Motion for unknown windowId={window_id}"
                        );
                        continue;
                    };
                    let scale = state.state.borrow().scale_factor;
                    let inputs = crate::events::translate_extra_motion_event(
                        action_masked,
                        action_index,
                        meta_state,
                        button_state,
                        vscroll,
                        hscroll,
                        &positions,
                        scale,
                    );
                    for input in inputs {
                        state.handle_input(input);
                    }
                }
            }
        }

        self.common.borrow_mut().extra_event_rx = Some(rx);
    }

    /// Pull every queued `InputEvent` off android-activity's iterator and
    /// route translatable ones into the primary gpui window. Returning
    /// `InputStatus::Handled` for our own events lets android-activity stop
    /// propagating them up the system input stack.
    ///
    /// Extra-window inputs are NOT routed here — each `ExtraWindowActivity`
    /// has its own input pipeline (`OnTouchListener` + `OnKeyListener`) that
    /// JNIs into `multi_window` directly via `NativeBridge`. This loop only
    /// concerns events that GameActivity's native input queue receives,
    /// which is the primary surface alone.
    fn drain_input_events(&self) {
        use android_activity::InputStatus;
        use android_activity::input::InputEvent;

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

        // Hook AChoreographer for vsync-aligned rendering. The callback
        // is delivered on this thread's looper as part of the same
        // event stream android-activity polls, so vsync arrivals
        // unblock `poll_events` naturally — we don't need to drive
        // refresh from a tight timeout.
        install_choreographer_callback();

        while self.common.borrow().running {
            // 100ms is the upper bound (idle / unfocused). Any of:
            //   - input event
            //   - vsync (`frame_callback` runs as a looper task, sets
            //     FRAME_PENDING)
            //   - an enqueued waker (background-thread runnable)
            //   - main-thread event from android-activity
            // returns earlier. With the Choreographer driving us, this
            // loop ticks at the panel's refresh rate (60Hz / 90Hz /
            // 120Hz / etc.) when active and falls to ~10Hz idle.
            self.android_app.poll_events(
                Some(std::time::Duration::from_millis(100)),
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

            // Drain JNI-side extra-window lifecycle / touch events.
            self.drain_extra_window_events();

            // Refresh on vsync (FRAME_PENDING set by Choreographer
            // callback) or after main-thread events that may have
            // changed state. gpui's window.refresh() short-circuits
            // when nothing is dirty, so calling on every iteration is
            // cheap; gating on FRAME_PENDING saves the dirty-bit check
            // when we know nothing's happened.
            if FRAME_PENDING.swap(false, Ordering::AcqRel) {
                let (primary, extras) = {
                    let common = self.common.borrow();
                    (
                        common.window.clone(),
                        common.extra_windows.values().cloned().collect::<Vec<_>>(),
                    )
                };
                if let Some(window_ptr) = primary {
                    window_ptr.refresh();
                }
                for window_ptr in extras {
                    window_ptr.refresh();
                }
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
        let primary_present = self.common.borrow().window.is_some();
        if primary_present {
            return self.open_extra_window(handle, options);
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
        let window =
            AndroidWindow::new(handle, options, gpu_context, appearance, self.android_app.clone());

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
        crate::clipboard::read(&self.android_app)
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        crate::clipboard::write(&self.android_app, item);
    }

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
