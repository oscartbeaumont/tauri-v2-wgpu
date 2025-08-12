// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{borrow::Cow, sync::Mutex};

use tauri::{async_runtime::block_on, Manager, RunEvent, WindowEvent};
use wgpu::rwh::HasWindowHandle;
use windows::Win32::{
    Foundation::{COLORREF, HWND, POINT, SIZE},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    },
    UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, UpdateLayeredWindow, GWL_EXSTYLE, ULW_ALPHA,
        WS_EX_LAYERED,
    },
};

// Learn more about Tauri commands at https://tauri.app/v1/guides/features/command
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();
            let size = window.inner_size()?;

            let handle = window.window_handle().unwrap();

            unsafe {
                let hwnd = match handle.as_raw() {
                    raw_window_handle::RawWindowHandle::Win32(raw) => {
                        HWND(raw.hwnd.get() as *mut _)
                    }
                    _ => todo!(),
                };

                // Get current extended style
                let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);

                // Add WS_EX_LAYERED for layered window support
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED.0 as isize);
            }

            let instance = wgpu::Instance::default();

            let surface = instance.create_surface(window).unwrap();
            let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            }))
            .expect("Failed to find an appropriate adapter");

            // Create the logical device and command queue
            let (device, queue) = block_on(
                adapter.request_device(
                    &wgpu::DeviceDescriptor {
                        label: None,
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                            .using_resolution(adapter.limits()),
                    },
                    None,
                ),
            )
            .expect("Failed to create device");

            // Create an intermediate texture for manual compositing
            let intermediate_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Intermediate Texture"),
                size: wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });

            // Create buffer for reading texture data back to CPU with proper alignment
            let unpadded_bytes_per_row = size.width * 4; // RGBA = 4 bytes per pixel
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let aligned_bytes_per_row = (unpadded_bytes_per_row + align - 1) / align * align;
            let buffer_size = aligned_bytes_per_row * size.height;

            let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Readback Buffer"),
                size: buffer_size as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            // Load the shaders
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
                    r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) in_vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    // Create fullscreen triangle
    let x = f32(i32(in_vertex_index) - 1);
    let y = f32(i32(in_vertex_index & 1u) * 2 - 1);

    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let center = vec2<f32>(0.5, 0.5);
    let dist = distance(in.uv, center);

    // Create content with varying alpha
    if (dist < 0.2) {
        // Solid red circle in center
        return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    } else if (dist < 0.35) {
        // Semi-transparent blue ring
        let alpha = (0.35 - dist) / 0.15;
        return vec4<f32>(0.0, 0.5, 1.0, alpha * 0.8);
    } else if (dist < 0.45) {
        // Fading green outer ring
        let alpha = (0.45 - dist) / 0.1;
        return vec4<f32>(0.0, 1.0, 0.0, alpha * 0.4);
    }

    // Transparent background
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}
"#,
                )),
            });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

            let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Main Render Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            // Configure surface (we'll use this for fallback)
            let swapchain_capabilities = surface.get_capabilities(&adapter);
            let swapchain_format = swapchain_capabilities.formats[0];
            let alpha_mode = wgpu::CompositeAlphaMode::Opaque;

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: swapchain_format,
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };

            surface.configure(&device, &config);

            app.manage(surface);
            app.manage(render_pipeline);
            app.manage(device);
            app.manage(queue);
            app.manage(intermediate_texture);
            app.manage(readback_buffer);
            app.manage(Mutex::new(config));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            match event {
                RunEvent::WindowEvent {
                    label: _,
                    event: WindowEvent::Resized(size),
                    ..
                } => {
                    let config = app_handle.state::<Mutex<wgpu::SurfaceConfiguration>>();
                    let surface = app_handle.state::<wgpu::Surface>();
                    let device = app_handle.state::<wgpu::Device>();

                    let mut config = config.lock().unwrap();
                    config.width = if size.width > 0 { size.width } else { 1 };
                    config.height = if size.height > 0 { size.height } else { 1 };
                    surface.configure(&device, &config);
                }
                RunEvent::MainEventsCleared => {
                    let render_pipeline = app_handle.state::<wgpu::RenderPipeline>();
                    let device = app_handle.state::<wgpu::Device>();
                    let queue = app_handle.state::<wgpu::Queue>();
                    let intermediate_texture = app_handle.state::<wgpu::Texture>();
                    let readback_buffer = app_handle.state::<wgpu::Buffer>();

                    // Render to intermediate texture
                    let view =
                        intermediate_texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let mut encoder = device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

                    {
                        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: None,
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color {
                                        r: 0.0,
                                        g: 0.0,
                                        b: 0.0,
                                        a: 0.0,
                                    }),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                        rpass.set_pipeline(&render_pipeline);
                        rpass.draw(0..3, 0..1);
                    }

                    // Copy texture to buffer
                    let config = app_handle.state::<Mutex<wgpu::SurfaceConfiguration>>();
                    let config = config.lock().unwrap();

                    // Calculate aligned bytes per row (required by WGPU)
                    let unpadded_bytes_per_row = config.width * 4; // RGBA = 4 bytes per pixel
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let aligned_bytes_per_row =
                        (unpadded_bytes_per_row + align - 1) / align * align;

                    encoder.copy_texture_to_buffer(
                        wgpu::ImageCopyTexture {
                            aspect: wgpu::TextureAspect::All,
                            texture: &*intermediate_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                        },
                        wgpu::ImageCopyBuffer {
                            buffer: &*readback_buffer,
                            layout: wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(aligned_bytes_per_row),
                                rows_per_image: Some(config.height),
                            },
                        },
                        wgpu::Extent3d {
                            width: config.width,
                            height: config.height,
                            depth_or_array_layers: 1,
                        },
                    );

                    queue.submit(Some(encoder.finish()));

                    // Read back data and perform manual compositing
                    let buffer_slice = readback_buffer.slice(..);
                    let (sender, receiver) = std::sync::mpsc::channel();
                    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
                        sender.send(result).unwrap();
                    });

                    device.poll(wgpu::Maintain::Wait);

                    if let Ok(Ok(())) = receiver.recv() {
                        let buffer_view = buffer_slice.get_mapped_range();

                        // Perform manual compositing with Windows API
                        if let Some(window) = app_handle.get_webview_window("main") {
                            manual_composite_window(
                                &window,
                                &buffer_view,
                                config.width,
                                config.height,
                                aligned_bytes_per_row,
                            );
                        }

                        drop(buffer_view);
                        readback_buffer.unmap();
                    }
                }
                _ => (),
            }
        });
}

fn manual_composite_window(
    window: &tauri::WebviewWindow,
    rgba_data: &[u8],
    width: u32,
    height: u32,
    bytes_per_row: u32,
) {
    unsafe {
        let handle = window.window_handle().unwrap();
        let hwnd = match handle.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(raw) => HWND(raw.hwnd.get() as *mut _),
            _ => return,
        };

        // Create memory DC for compositing
        let desktop_dc = GetDC(Some(HWND(std::ptr::null_mut())));
        let mem_dc = CreateCompatibleDC(Some(desktop_dc));

        // Create a DIB for our RGBA data
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // Negative for top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default(); 1],
        };

        let mut dib_bits: *mut std::ffi::c_void = std::ptr::null_mut();

        if let Ok(dib) =
            CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut dib_bits, None, 0)
        {
            if !dib_bits.is_null() {
                // Convert RGBA to BGRA and premultiply alpha
                let dib_slice = std::slice::from_raw_parts_mut(
                    dib_bits as *mut u8,
                    (width * height * 4) as usize,
                );

                // Handle aligned buffer data
                for y in 0..height {
                    for x in 0..width {
                        let src_offset = (y * bytes_per_row + x * 4) as usize;
                        let dst_offset = (y * width * 4 + x * 4) as usize;

                        if src_offset + 3 < rgba_data.len() && dst_offset + 3 < dib_slice.len() {
                            let r = rgba_data[src_offset] as f32 / 255.0;
                            let g = rgba_data[src_offset + 1] as f32 / 255.0;
                            let b = rgba_data[src_offset + 2] as f32 / 255.0;
                            let a = rgba_data[src_offset + 3] as f32 / 255.0;

                            // Premultiply alpha and convert to BGRA
                            dib_slice[dst_offset] = (b * a * 255.0) as u8; // B
                            dib_slice[dst_offset + 1] = (g * a * 255.0) as u8; // G
                            dib_slice[dst_offset + 2] = (r * a * 255.0) as u8; // R
                            dib_slice[dst_offset + 3] = (a * 255.0) as u8; // A
                        }
                    }
                }

                // Select DIB into memory DC
                let old_obj = SelectObject(mem_dc, dib.into());

                // Update layered window
                let window_size = SIZE {
                    cx: width as i32,
                    cy: height as i32,
                };
                let source_pos = POINT { x: 0, y: 0 };

                let blend_func = BLENDFUNCTION {
                    BlendOp: 0, // AC_SRC_OVER
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: 1, // AC_SRC_ALPHA
                };

                let _ = UpdateLayeredWindow(
                    hwnd,
                    Some(desktop_dc),
                    None,
                    Some(&window_size),
                    Some(mem_dc),
                    Some(&source_pos),
                    COLORREF(0),
                    Some(&blend_func),
                    ULW_ALPHA,
                );

                // Cleanup
                let _ = SelectObject(mem_dc, old_obj);

                let _ = DeleteObject(dib.into());
            }
        }

        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, desktop_dc);
    }
}
