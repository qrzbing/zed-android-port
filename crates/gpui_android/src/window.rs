use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use futures::channel::oneshot;
use ndk::native_window::NativeWindow;
use raw_window_handle as rwh;

use gpui::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, GpuSpecs, Modifiers,
    PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Pixels,
    Point, PromptButton, PromptLevel, RequestFrameOptions, Scene, Size, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowParams, point, px, size,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

use crate::display::AndroidDisplay;

/// Raw window handle wrapper for Android. Holds a `*mut ANativeWindow` pointer
/// (obtained from `android_activity::AndroidApp::native_window()`), or null when
/// the surface is not currently attached (between `TerminateWindow` and the next
/// `InitWindow`).
///
/// `Send + Sync` are required by `WgpuRenderer::new`'s bounds; the pointer is
/// only ever dereferenced on the main thread, and wgpu uses it synchronously
/// during surface creation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AndroidRawWindow {
    pub(crate) native_window: *mut c_void,
}

unsafe impl Send for AndroidRawWindow {}
unsafe impl Sync for AndroidRawWindow {}

impl rwh::HasWindowHandle for AndroidRawWindow {
    fn window_handle(&self) -> std::result::Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let Some(non_null) = NonNull::new(self.native_window) else {
            return Err(rwh::HandleError::Unavailable);
        };
        let handle = rwh::AndroidNdkWindowHandle::new(non_null);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasDisplayHandle for AndroidRawWindow {
    fn display_handle(&self) -> std::result::Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let handle = rwh::AndroidDisplayHandle::new();
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

#[derive(Default)]
pub(crate) struct Callbacks {
    pub(crate) request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    pub(crate) input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    pub(crate) active_status_change: Option<Box<dyn FnMut(bool)>>,
    pub(crate) hovered_status_change: Option<Box<dyn FnMut(bool)>>,
    pub(crate) resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    pub(crate) moved: Option<Box<dyn FnMut()>>,
    pub(crate) should_close: Option<Box<dyn FnMut() -> bool>>,
    pub(crate) close: Option<Box<dyn FnOnce()>>,
    pub(crate) appearance_changed: Option<Box<dyn FnMut()>>,
}

pub(crate) struct AndroidWindowState {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) renderer: Option<WgpuRenderer>,
    pub(crate) raw_window: AndroidRawWindow,
    pub(crate) display: Rc<dyn PlatformDisplay>,
    pub(crate) input_handler: Option<PlatformInputHandler>,
    pub(crate) appearance: WindowAppearance,
    pub(crate) background_appearance: WindowBackgroundAppearance,
    pub(crate) handle: AnyWindowHandle,
    pub(crate) gpu_context: GpuContext,
    /// Holds an `ANativeWindow_acquire` refcount on the underlying native
    /// window so the pointer stored in `raw_window` stays valid for the
    /// lifetime of any Vulkan `VkSurfaceKHR` referencing it. Dropped (and
    /// thus refcount-decremented) on `detach_surface` and replaced when
    /// `attach_surface` runs with a fresh window.
    pub(crate) native_window: Option<NativeWindow>,
}

#[derive(Clone)]
pub(crate) struct AndroidWindowStatePtr {
    pub(crate) state: Rc<RefCell<AndroidWindowState>>,
    pub(crate) callbacks: Rc<RefCell<Callbacks>>,
}

impl AndroidWindowStatePtr {
    /// Called from the platform run loop on `MainEvent::InitWindow` (and from
    /// `open_window` on first attach). Wires the new `ANativeWindow` surface
    /// into the renderer, creating it on first call and replacing the surface
    /// on subsequent calls (e.g. after a rotation that destroys and recreates
    /// the window).
    ///
    /// On first call, this is also where the shared `WgpuContext` (device,
    /// queue, adapter) gets created — the gpu_context cell starts empty and
    /// `WgpuRenderer::new` populates it.
    ///
    /// Takes ownership of `NativeWindow` so the underlying refcount on
    /// `ANativeWindow*` is held for the life of this surface; the wrapper is
    /// dropped on `detach_surface` or replaced on subsequent `attach_surface`.
    pub(crate) fn attach_surface(&self, native_window: NativeWindow) -> Result<()> {
        let width = native_window.width() as u32;
        let height = native_window.height() as u32;
        let raw_window = AndroidRawWindow {
            native_window: native_window.ptr().as_ptr().cast(),
        };
        let config = WgpuSurfaceConfig {
            size: size(
                DevicePixels(width.max(1) as i32),
                DevicePixels(height.max(1) as i32),
            ),
            transparent: false,
            preferred_present_mode: None,
        };

        let mut state = self.state.borrow_mut();
        state.raw_window = raw_window;
        state.bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(width as f32), px(height as f32)),
        };

        if state.renderer.is_some() {
            let gpu_context = state.gpu_context.clone();
            let ctx_ref = gpu_context.borrow();
            let instance = ctx_ref
                .as_ref()
                .map(|ctx| ctx.instance.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!("attach_surface: gpu_context missing on re-attach")
                })?;
            drop(ctx_ref);
            state
                .renderer
                .as_mut()
                .unwrap()
                .replace_surface(&raw_window, config, &instance)?;
            log::info!("AndroidWindow::attach_surface: replaced surface ({width}x{height})");
        } else {
            let gpu_context = state.gpu_context.clone();
            let renderer = WgpuRenderer::new(gpu_context, &raw_window, config, None)?;
            state.renderer = Some(renderer);
            log::info!("AndroidWindow::attach_surface: created renderer ({width}x{height})");
        }

        // Replace the previous wrapper (if any). The drop releases the prior
        // ANativeWindow refcount; Vulkan's VkSurfaceKHR holds its own ref.
        state.native_window = Some(native_window);
        Ok(())
    }

    /// Called from the platform run loop on `MainEvent::TerminateWindow`. The
    /// `ANativeWindow` is being destroyed by the system. We unconfigure the
    /// wgpu surface so subsequent draws bail out, but keep the device, queue,
    /// and atlas alive so the next `InitWindow` can `replace_surface` cheaply
    /// without rebuilding glyph caches.
    pub(crate) fn detach_surface(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(renderer) = state.renderer.as_mut() {
            renderer.unconfigure_surface();
        }
        state.raw_window = AndroidRawWindow {
            native_window: std::ptr::null_mut(),
        };
        // Drop the wrapper to release our ANativeWindow refcount.
        state.native_window = None;
        log::info!("AndroidWindow::detach_surface: surface unconfigured");
    }

    /// Called on `MainEvent::WindowResized`. Updates the drawable size on the
    /// renderer and the cached bounds.
    pub(crate) fn resize_surface(&self, width: u32, height: u32) {
        let mut state = self.state.borrow_mut();
        state.bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(width as f32), px(height as f32)),
        };
        if let Some(renderer) = state.renderer.as_mut() {
            renderer.update_drawable_size(size(
                DevicePixels(width.max(1) as i32),
                DevicePixels(height.max(1) as i32),
            ));
        }
    }

    /// Drive a paint cycle: invoke the `request_frame` callback that gpui
    /// registered. The callback walks the element tree, builds a `Scene`, and
    /// calls back into our `PlatformWindow::draw` to actually submit it.
    ///
    /// The take/restore dance mirrors X11's `refresh` — re-entrant `refresh`
    /// calls during the callback's own paint side-effects would otherwise
    /// double-borrow the callback Box.
    pub(crate) fn refresh(&self) {
        let callback = self.callbacks.borrow_mut().request_frame.take();
        if let Some(mut callback) = callback {
            callback(RequestFrameOptions {
                require_presentation: false,
                force_render: false,
            });
            self.callbacks.borrow_mut().request_frame = Some(callback);
        }
    }
}

pub(crate) struct AndroidWindow(pub(crate) AndroidWindowStatePtr);

impl AndroidWindow {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        _params: WindowParams,
        gpu_context: GpuContext,
        appearance: WindowAppearance,
    ) -> Self {
        let display: Rc<dyn PlatformDisplay> = Rc::new(AndroidDisplay::new());
        let bounds = display.bounds();

        let state = AndroidWindowState {
            bounds,
            scale_factor: 1.0,
            renderer: None,
            raw_window: AndroidRawWindow {
                native_window: std::ptr::null_mut(),
            },
            display,
            input_handler: None,
            appearance,
            background_appearance: WindowBackgroundAppearance::Opaque,
            handle,
            gpu_context,
            native_window: None,
        };

        Self(AndroidWindowStatePtr {
            state: Rc::new(RefCell::new(state)),
            callbacks: Rc::new(RefCell::new(Callbacks::default())),
        })
    }

    pub(crate) fn ptr(&self) -> AndroidWindowStatePtr {
        self.0.clone()
    }
}

impl rwh::HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> std::result::Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let raw = self.0.state.borrow().raw_window;
        let Some(non_null) = NonNull::new(raw.native_window) else {
            return Err(rwh::HandleError::Unavailable);
        };
        let handle = rwh::AndroidNdkWindowHandle::new(non_null);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> std::result::Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let handle = rwh::AndroidDisplayHandle::new();
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.state.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Maximized(self.0.state.borrow().bounds)
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.state.borrow().bounds.size
    }

    fn resize(&mut self, _size: Size<Pixels>) {}

    fn scale_factor(&self) -> f32 {
        self.0.state.borrow().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        self.0.state.borrow().appearance
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.0.state.borrow().display.clone())
    }

    fn mouse_position(&self) -> Point<Pixels> {
        Point::default()
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.state.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.state.borrow_mut().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {}

    fn is_active(&self) -> bool {
        true
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        self.0.state.borrow().background_appearance
    }

    fn set_title(&mut self, _title: &str) {}

    fn set_background_appearance(&self, _bg: WindowBackgroundAppearance) {}

    fn minimize(&self) {}
    fn zoom(&self) {}
    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.0.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().hovered_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().moved = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_hit_test_window_control(
        &self,
        _callback: Box<dyn FnMut() -> Option<WindowControlArea>>,
    ) {
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        let mut state = self.0.state.borrow_mut();
        let raw_window = state.raw_window;
        let Some(renderer) = state.renderer.as_mut() else {
            return;
        };

        if renderer.device_lost() {
            if raw_window.native_window.is_null() {
                log::warn!("draw: device lost but no native window to recover against");
                return;
            }
            if let Err(err) = renderer.recover(&raw_window) {
                log::error!("GPU recovery failed: {err:#}");
            }
            return;
        }

        renderer.draw(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        // `gpui::Window::new` calls this once during construction. `open_window`
        // blocks until `attach_surface` succeeds, so the renderer (and its atlas)
        // is always populated by the time gpui asks for it.
        self.0
            .state
            .borrow()
            .renderer
            .as_ref()
            .expect("sprite_atlas: open_window must attach surface before returning")
            .sprite_atlas()
            .clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.0
            .state
            .borrow()
            .renderer
            .as_ref()
            .map(|r| r.gpu_specs())
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {}
}
