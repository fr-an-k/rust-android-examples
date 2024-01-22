// To turn off console in Windows build:
//#![windows_subsystem = "windows"]

use std::{borrow::Cow, sync::Arc};

use android_activity::WindowManagerFlags;
use log::trace;

use wgpu::TextureFormat;
use wgpu::{Adapter, Device, Instance, PipelineLayout, Queue, RenderPipeline, ShaderModule};

use winit::{
    event::{Event, StartCause::WaitCancelled, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopWindowTarget},
};

#[cfg(target_os = "android")]
use winit::platform::android::{activity::AndroidApp, EventLoopBuilderExtAndroid};

struct RenderState {
    device: Device,
    queue: Queue,
    _shader: ShaderModule,
    target_format: TextureFormat,
    _pipeline_layout: PipelineLayout,
    render_pipeline: RenderPipeline,
}

struct SurfaceState<'a> {
    window: Arc<winit::window::Window>,
    surface: wgpu::Surface<'a>,
}

struct App<'a> {
    instance: Instance,
    adapter: Option<Adapter>,
    surface_state: Option<SurfaceState<'a>>,
    render_state: Option<RenderState>,
    #[cfg(target_os = "android")]
    android_app: Option<AndroidApp>,
}

impl App<'_> {
    fn new(instance: Instance) -> Self {
        Self {
            instance,
            adapter: None,
            surface_state: None,
            render_state: None,
            #[cfg(target_os = "android")]
            android_app: None,
        }
    }

    fn create_surface<T>(&mut self, elwt: &EventLoopWindowTarget<T>) {
        #[cfg(target_arch = "wasm32")]
        let window = {
            use winit::{dpi::PhysicalSize, platform::web::WindowBuilderExtWebSys};
            Arc::new(
                winit::window::WindowBuilder::new()
                    // Automatically creates the canvas with [data-raw-handle] suitable for wgpu
                    .with_canvas(None)
                    // Winit prevents sizing with CSS, so we have to set
                    // the size manually when on web.
                    .with_inner_size(PhysicalSize::new(450, 400))
                    .with_append(true)
                    .build(elwt)
                    .unwrap(),
            )
        };
        // For other platforms you could also use the WindowBuilder to set the title etc.
        #[cfg(not(target_arch = "wasm32"))]
        let window = Arc::new(winit::window::Window::new(elwt).unwrap());

        log::info!("WGPU: creating surface for native window");
        let surface = self
            .instance
            .create_surface(window.clone())
            .expect("Failed to create surface");
        self.surface_state = Some(SurfaceState { window, surface });
    }

    async fn init_render_state(adapter: &Adapter, target_format: TextureFormat) -> RenderState {
        log::info!("Initializing render state");

        log::info!(
            "WebGPU compliant adapter? {}",
            adapter.get_downlevel_capabilities().is_webgpu_compliant()
        );
        log::info!("Supports: {:?}", adapter.features());

        log::info!("WGPU: requesting device");
        // Create the logical device and command queue
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: wgpu::Features::empty(),
                    // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the swapchain.
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                        .using_resolution(adapter.limits()),
                },
                None,
            )
            .await
            .expect("Failed to create device");

        log::info!("WGPU: loading shader");
        // Load the shaders from disk
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shader.wgsl"))),
        });

        log::info!("WGPU: creating pipeline layout");
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        log::info!("WGPU: creating render pipeline");
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(target_format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        RenderState {
            device,
            queue,
            _shader: shader,
            target_format,
            _pipeline_layout: pipeline_layout,
            render_pipeline,
        }
    }

    // We want to defer the initialization of our render state until
    // we have a surface so we can take its format into account.
    //
    // After we've initialized our render state once though we
    // expect all future surfaces will have the same format and we
    // so this stat will remain valid.
    async fn ensure_render_state_for_surface(&mut self) {
        if let Some(surface_state) = &self.surface_state {
            if self.adapter.is_none() {
                log::info!("WGPU: requesting a suitable adapter (compatible with our surface)");
                let adapter = self
                    .instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::default(),
                        force_fallback_adapter: false,
                        // Request an adapter which can render to our surface
                        compatible_surface: Some(&surface_state.surface),
                    })
                    .await
                    .expect("Failed to find an appropriate adapter");

                self.adapter = Some(adapter);
            }
            let adapter = self.adapter.as_ref().unwrap();

            if self.render_state.is_none() {
                log::info!("WGPU: finding supported swapchain format");
                let surface_caps = surface_state.surface.get_capabilities(adapter);

                let swapchain_format = surface_caps
                    .formats
                    .iter()
                    .copied()
                    .find(|f| f.is_srgb())
                    .unwrap_or(surface_caps.formats[0]);

                let rs = Self::init_render_state(adapter, swapchain_format).await;
                self.render_state = Some(rs);
            }
        }
    }

    fn configure_surface_swapchain(&mut self) {
        if let (Some(render_state), Some(surface_state)) = (&self.render_state, &self.surface_state)
        {
            let swapchain_format = render_state.target_format;
            let size = surface_state.window.inner_size();

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: swapchain_format,
                width: size.width,
                height: size.height,
                desired_maximum_frame_latency: 2,
                //present_mode: wgpu::PresentMode::Mailbox,
                present_mode: wgpu::PresentMode::Fifo,
                view_formats: vec![swapchain_format],
                alpha_mode: wgpu::CompositeAlphaMode::Inherit,
                //alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            };

            log::info!("WGPU: Configuring surface swapchain: format = {swapchain_format:?}, size = {size:?}");
            surface_state
                .surface
                .configure(&render_state.device, &config);
        }
    }

    fn queue_redraw(&self) {
        if let Some(surface_state) = &self.surface_state {
            trace!("Making Redraw Request");
            surface_state.window.request_redraw();
        }
    }

    async fn resume<T>(&mut self, event_loop: &EventLoopWindowTarget<T>) {
        self.create_surface(event_loop);
        self.ensure_render_state_for_surface().await;
        self.configure_surface_swapchain();
        self.queue_redraw();
    }

    fn render(&mut self) {
        if let Some(ref surface_state) = self.surface_state {
            if let Some(ref rs) = self.render_state {
                let frame = surface_state
                    .surface
                    .get_current_texture()
                    .expect("Failed to acquire next swap chain texture");
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder = rs
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                {
                    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });
                    rpass.set_pipeline(&rs.render_pipeline);
                    rpass.draw(0..3, 0..1);
                }

                rs.queue.submit(Some(encoder.finish()));
                frame.present();

                // To animate, uncomment this to request the next frame:
                //surface_state.window.request_redraw();
            }
        }
    }
}

fn run(event_loop: EventLoop<()>, mut app: App) {
    log::info!("Running mainloop...");
    event_loop.set_control_flow(ControlFlow::Wait);

    event_loop
        .run(move |event, elwt| {
            match event {
                Event::Resumed => {
                    log::info!("Resumed, creating render state...");
                    #[cfg(not(target_arch = "wasm32"))]
                    pollster::block_on(app.resume(&elwt));
                }
                Event::Suspended => {
                    log::info!("Suspended, dropping render state...");
                    app.render_state = None;
                }
                Event::WindowEvent {
                    event: WindowEvent::Resized(_size),
                    ..
                } => {
                    app.configure_surface_swapchain();
                    // Winit: doesn't currently implicitly request a redraw
                    // for a resize which may be required on some platforms...
                    app.queue_redraw();
                }
                Event::WindowEvent {
                    event: WindowEvent::RedrawRequested,
                    ..
                } => {
                    log::info!("Handling Redraw Request");
                    app.render();
                }
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    ..
                } => elwt.exit(),
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CursorMoved { .. } => {
                        // not logged, contains mouse motion
                    }
                    #[cfg(target_os = "android")]
                    WindowEvent::Touch { .. } => {
                        // Demonstration of showing onscreen keyboard.
                        // show_implicit argument means something other than
                        // a literal "open keyboard" button was pressed
                        log::info!("check");
                        app.android_app.as_ref().unwrap().show_soft_input(false);
                    }
                    _ => {
                        log::info!("Window event {:#?}", event);
                    }
                },
                Event::AboutToWait => {
                    // not logged
                }
                Event::NewEvents(WaitCancelled {
                    start: _,
                    requested_resume: _,
                }) => {
                    // not logged
                }
                Event::DeviceEvent {
                    device_id: _,
                    event: _,
                } => {
                    // not logged, contains mouse motion
                }
                _ => {
                    log::info!("Unhandled event: {event:?}");
                }
            }
        })
        .ok();
}

async fn _main(#[cfg(target_os = "android")] android_app: AndroidApp) {
    let wgpu_backend = option_env!("WGPU_BACKEND");
    let backends = if wgpu_backend != None {
        log::info!("Using wgpu backend(s) {}", wgpu_backend.unwrap());
        wgpu::util::parse_backends_from_comma_list(wgpu_backend.unwrap())
    } else {
        log::info!("Using any WGPU backend");
        wgpu::Backends::all()
    };

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    #[allow(unused_mut)]
    let mut app = App::new(instance);

    // spawn_local causes ownership troubles in the event loop closure
    // so just create the surface here
    #[cfg(target_arch = "wasm32")]
    app.resume(&event_loop).await;

    #[cfg(target_os = "android")]
    let event_loop = {
        //use android_activity::WindowManagerFlags;
        /*android_app.set_window_flags(
            WindowManagerFlags::empty(),
            WindowManagerFlags::NOT_FOCUSABLE | WindowManagerFlags::NOT_TOUCH_MODAL,
        );
        android_app.show_soft_input(false);*/
        /*View decorView = getWindow().getDecorView();
        WindowInsetsControllerCompat controller = new WindowInsetsControllerCompat(getWindow(),
                decorView);
        controller.show(WindowInsetsCompat.Type.ime());*/

        let activity = android_app.activity_as_ptr();
        ndk_sys::ANativeActivity_

        app.android_app = Some(android_app.clone());
        log::info!(
            "Android app internal data path {}",
            android_app.internal_data_path().unwrap().display()
        );
        log::info!(
            "Android app external data path {}",
            android_app.external_data_path().unwrap().display()
        );
        /*use jni;
        use ndk_context;
        let ctx = ndk_context::android_context();
        let jvm_ptr = ctx.vm();
        let jvm = unsafe {
            jni::JavaVM::from_raw(jvm_ptr.cast())
                .expect("Expected to find JVM via ndk_context crate")
        };
        let env = jvm.attach_current_thread_permanently().unwrap();

        let activity_ptr = ctx.context();
        let activity =
            unsafe { jni::objects::JObject::from_raw(activity_ptr as jni::sys::jobject) };*/

        EventLoopBuilder::new()
            .with_android_app(android_app)
            .build()
            .unwrap()
    };

    #[cfg(not(target_os = "android"))]
    let event_loop = EventLoopBuilder::new().build().unwrap();

    run(event_loop, app)
}

#[allow(dead_code)]
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    pollster::block_on(_main(app));
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    console_log::init_with_level(log::Level::Info).expect("Couldn't initialize logger");

    wasm_bindgen_futures::spawn_local(_main());
}

#[allow(dead_code)]
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info) // Default Log Level
        .parse_default_env()
        .init();

    pollster::block_on(_main());
}
