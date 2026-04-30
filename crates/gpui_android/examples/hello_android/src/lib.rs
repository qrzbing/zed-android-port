#![cfg(target_os = "android")]

use android_activity::{AndroidApp, MainEvent, PollEvent};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use log::{error, info};
use ndk::native_window::NativeWindow;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
};

const BUNDLED_FONT: &[u8] =
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Regular.ttf");

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

    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: Buffer,
}

impl Renderer {
    fn new(window: NativeWindow) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let raw_window =
            RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(window.ptr().cast()));
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
        }))?;
        info!("wgpu adapter: {:?}", adapter.get_info());

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hello_android device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))?;

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

        let mut font_system = FontSystem::new();
        font_system.db_mut().load_font_data(BUNDLED_FONT.to_vec());
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );

        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(80.0, 100.0));
        text_buffer.set_size(
            &mut font_system,
            Some(config.width as f32),
            Some(config.height as f32),
        );
        text_buffer.set_text(
            &mut font_system,
            "Hello, Android!",
            &Attrs::new().family(Family::Name("Lilex")),
            Shaping::Advanced,
        );
        text_buffer.shape_until_scroll(&mut font_system, false);

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            config,
            frame: 0,
            font_system,
            swash_cache,
            viewport,
            text_atlas,
            text_renderer,
            text_buffer,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.text_buffer
            .set_size(&mut self.font_system, Some(width as f32), Some(height as f32));
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);
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

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.text_atlas,
                &self.viewport,
                [TextArea {
                    buffer: &self.text_buffer,
                    left: 80.0,
                    top: 80.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: self.config.width as i32,
                        bottom: self.config.height as i32,
                    },
                    default_color: TextColor::rgb(255, 255, 255),
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .map_err(|e| anyhow::anyhow!("text prepare: {e:?}"))?;

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear+text pass"),
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
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut rpass)
                .map_err(|e| anyhow::anyhow!("text render: {e:?}"))?;
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }
}
