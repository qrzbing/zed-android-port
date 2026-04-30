#![cfg(target_os = "android")]

use android_activity::{AndroidApp, MainEvent, PollEvent};
use log::{error, info};
use ndk::native_window::NativeWindow;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("hello_android"),
    );
    info!("android_main: bootstrap");

    let mut renderer: Option<Renderer> = None;
    let mut quit = false;

    while !quit {
        app.poll_events(Some(std::time::Duration::from_millis(16)), |event| match event {
            PollEvent::Main(main_event) => {
                info!("main event: {main_event:?}");
                match main_event {
                    MainEvent::InitWindow { .. } => {
                        if let Some(window) = app.native_window() {
                            match Renderer::new(window) {
                                Ok(r) => {
                                    info!("renderer initialized");
                                    renderer = Some(r);
                                }
                                Err(e) => error!("renderer init failed: {e:#}"),
                            }
                        }
                    }
                    MainEvent::TerminateWindow { .. } => {
                        info!("dropping renderer");
                        renderer = None;
                    }
                    MainEvent::WindowResized { .. } => {
                        if let (Some(r), Some(w)) = (renderer.as_mut(), app.native_window()) {
                            r.resize(w.width() as u32, w.height() as u32);
                        }
                    }
                    MainEvent::RedrawNeeded { .. } => {
                        if let Some(r) = renderer.as_mut() {
                            if let Err(e) = r.render() {
                                error!("render error: {e:#}");
                            }
                        }
                    }
                    MainEvent::Destroy => quit = true,
                    _ => {}
                }
            }
            _ => {}
        });

        if let Some(r) = renderer.as_mut() {
            if let Err(e) = r.render() {
                error!("render error: {e:#}");
            }
        }
    }
}

struct Renderer {
    _window: NativeWindow,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    frame: u32,
}

impl Renderer {
    fn new(window: NativeWindow) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let raw_window = RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(window.ptr().cast()));
        let raw_display = RawDisplayHandle::Android(AndroidDisplayHandle::new());

        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: raw_display,
                raw_window_handle: raw_window,
            })
        }?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| anyhow::anyhow!("no compatible wgpu adapter"))?;
        info!("wgpu adapter: {:?}", adapter.get_info());

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("hello_android device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: (window.width() as u32).max(1),
            height: (window.height() as u32).max(1),
            present_mode: caps.present_modes[0],
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            config,
            frame: 0,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self) -> anyhow::Result<()> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hello_android encoder"),
            });

        let t = self.frame as f64 / 120.0;
        let r = t.sin() * 0.5 + 0.5;
        let g = (t + 2.094).sin() * 0.5 + 0.5;
        let b = (t + 4.188).sin() * 0.5 + 0.5;

        {
            let _rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }
}
