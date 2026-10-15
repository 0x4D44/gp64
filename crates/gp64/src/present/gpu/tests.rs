use super::*;

#[test]
fn wgsl_parses_and_validates() {
    for (name, source) in SHADERS
        .iter()
        .enumerate()
        .map(|(i, source)| (["Integer", "Smooth", "Area"][i], scaling_shader(source)))
        .chain(std::iter::once(("Overlay", OVERLAY_SHADER.to_owned())))
    {
        let module = wgpu::naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{name} WGSL parse failed: {error}"));
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name} WGSL validation failed: {error}"));
    }
}

// Mirror of integer.wgsl:source_coordinate, with the production viewport fed
// into the shader's uniform. Offscreen readback below also exercises WGSL.
fn source_coordinate(pixel: [usize; 2], source: [usize; 2], view: &Viewport) -> [usize; 2] {
    [
        (pixel[0] - view.x) * source[0] / view.width,
        (pixel[1] - view.y) * source[1] / view.height,
    ]
}

#[test]
fn integer_coordinates_match_cpu() {
    for (sw, sh) in [
        (1, 1),
        (2, 3),
        (3, 2),
        (7, 5),
        (8, 9),
        (319, 199),
        (320, 200),
        (640, 350),
        (720, 400),
    ] {
        let source: Vec<u32> = (0..sw * sh).map(|i| i as u32 + 1).collect();
        for (width, height) in [
            (1, 1),
            (1, 7),
            (7, 1),
            (2, 3),
            (13, 7),
            (79, 61),
            (159, 119),
            (320, 240),
            (641, 481),
            (997, 731),
            (701, 999),
        ] {
            let view = scaler::viewport(sw, width, height, Scaler::Integer);
            let mut expected = vec![0; width * height];
            Blitter::default().blit(
                &source,
                sw,
                sh,
                &mut expected,
                width,
                height,
                Scaler::Integer,
            );
            for y in 0..height {
                for x in 0..width {
                    let actual = if x >= view.x
                        && x < view.x + view.width
                        && y >= view.y
                        && y < view.y + view.height
                    {
                        let [sx, sy] = source_coordinate([x, y], [sw, sh], &view);
                        source[sy * sw + sx]
                    } else {
                        0
                    };
                    assert_eq!(
                        actual,
                        expected[y * width + x],
                        "{sw}x{sh} -> {width}x{height} at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
fn software_override_and_failed_init_choose_fallback() {
    assert_eq!(
        super::super::try_gpu(true, || -> Result<u8> {
            panic!("forced software must not initialize GPU")
        }),
        None
    );
    assert_eq!(
        super::super::try_gpu(false, || -> Result<u8> {
            bail!("injected adapter failure")
        }),
        None
    );
    assert_eq!(super::super::try_gpu(false, || Ok(7)), Some(7));
}

#[track_caller]
fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    width: u32,
    height: u32,
    view: &Viewport,
) -> Vec<u32> {
    readback_with(
        device,
        queue,
        renderer,
        width,
        height,
        view,
        Scaler::Integer,
        false,
    )
}

#[track_caller]
fn readback_with(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    width: u32,
    height: u32,
    view: &Viewport,
    scaler: Scaler,
    gpu_overlays: bool,
) -> Vec<u32> {
    let caller = std::panic::Location::caller();
    eprintln!(
        "  mapping GPU output {width}x{height} ({scaler:?}, overlays={gpu_overlays}) from {caller}; viewport=({}, {}) {}x{}",
        view.x, view.y, view.width, view.height
    );
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gp64 offscreen oracle"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    queue.submit([renderer.draw(
        device,
        queue,
        &output.create_view(&Default::default()),
        view,
        scaler,
        gpu_overlays,
    )]);
    let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gp64 readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        output.size(),
    );
    queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })
        .unwrap_or_else(|error| panic!("GPU readback poll failed for {width}x{height}: {error}"));
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap_or_else(|error| {
            panic!("GPU readback callback timed out for {width}x{height}: {error}")
        })
        .unwrap_or_else(|error| {
            panic!("GPU readback mapping failed for {width}x{height}: {error}")
        });
    let mapped = buffer.slice(..).get_mapped_range();
    mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| {
            row[..width as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    assert_eq!(p[3], 255, "GPU output must be opaque");
                    u32::from_le_bytes(*p) & 0xffffff
                })
        })
        .collect()
}

fn offscreen_device(reason: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = instance();
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))
    .or_else(|fallback| {
        eprintln!("Fallback adapter unavailable: {fallback}; trying native adapter");
        block_on(instance.request_adapter(&Default::default()))
    });
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(error) => {
            eprintln!("SKIP {reason}: no headless adapter: {error}");
            return None;
        }
    };
    eprintln!("Offscreen {reason} running on {:?}", adapter.get_info());
    Some(
        block_on(adapter.request_device(&Default::default()))
            .expect("available adapter must create a device"),
    )
}

#[test]
fn offscreen_scalers_overlays_and_cpu_composition_match_software() {
    let Some((device, queue)) = offscreen_device("GPU comparison") else {
        return;
    };
    let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);

    let cases = [
        // Single-pixel axes, tiny windows, and ordinary upscales.
        (1, 1, 4, 3, 0),
        (1, 7, 13, 7, 1),
        (7, 1, 7, 13, 1),
        (7, 5, 13, 7, 1),
        (13, 7, 79, 61, 0),
        // Odd dimensions and letterboxes on both axes.
        (319, 199, 641, 481, 0),
        (320, 200, 1200, 600, 1),
        (320, 200, 601, 901, 1),
        // Non-integer and strong reductions.
        (1023, 769, 79, 61, 0),
        (1023, 769, 5, 3, 1),
        // The wide source stresses 64-bit weighted sums and reciprocal math.
        (8192, 9, 5, 3, 2),
        // The first 2048-column output footprint is half white and must round
        // 127.5 upward; this depends on AreaPlan's ceiling reciprocal.
        (8192, 9, 5, 3, 3),
        // A constant frame whose per-pixel weighted channel sum exceeds u32.
        (8192, 2057, 640, 480, 5),
        // At the interior pixel the exact one-round bilinear result is zero;
        // rounding after each axis would produce one in the red channel.
        (2, 2, 4, 3, 4),
        // One-pixel output dimensions must leave a well-defined frame.
        (7, 5, 1, 7, 0),
        (7, 5, 7, 1, 0),
    ];
    for (index, (sw, sh, width, height, pattern)) in cases.into_iter().enumerate() {
        let generation = index as u64 + 1;
        let source = match pattern {
            2 => vec![0x00d3_7a29; sw * sh],
            3 => (0..sw * sh)
                .map(|i| if i % sw >= sw / 8 { 0x00ff_ffff } else { 0 })
                .collect(),
            4 => vec![0, 0x0002_0000, 0, 0],
            5 => vec![0x00ff_ffff; sw * sh],
            _ => (0..sw * sh)
                .map(|i| {
                    let x = i % sw;
                    let y = i / sw;
                    let r = (x * 37 + y * 17 + 13) & 255;
                    let g = (x * 11 + y * 53 + 71) & 255;
                    let b = (x * 97 + y * 7 + 29) & 255;
                    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
                })
                .collect(),
        };
        renderer
            .upload(
                &device,
                &queue,
                &source,
                sw as u32,
                sh as u32,
                Some(generation),
            )
            .unwrap();
        for scaler in [Scaler::Integer, Scaler::Smooth, Scaler::Area] {
            if pattern == 5 && scaler != Scaler::Area {
                continue;
            }
            eprintln!(
                "GPU oracle case: {scaler:?}, source {sw}x{sh} -> target {width}x{height}, pattern={pattern}"
            );
            let view = scaler::viewport(sw, width, height, scaler);
            let actual = readback_with(
                &device,
                &queue,
                &renderer,
                width as u32,
                height as u32,
                &view,
                scaler,
                false,
            );
            let mut expected = vec![0; width * height];
            Blitter::default().blit(&source, sw, sh, &mut expected, width, height, scaler);
            if pattern == 3 && scaler == Scaler::Area {
                assert_eq!(
                    expected[0], 0x0080_8080,
                    "Area half-white average rounds up"
                );
            }
            assert_pixels_equal(
                &format!("GPU {scaler:?}: {sw}x{sh} -> {width}x{height}"),
                &actual,
                &expected,
                width,
            );
        }
    }

    // Unchanged generations do not upload. A generation update does, even
    // with unchanged storage/geometry; a CPU-composited upload invalidates it.
    let full = Viewport {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    renderer
        .upload(&device, &queue, &[0x123456; 4], 2, 2, Some(100))
        .unwrap();
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(100))
        .unwrap();
    assert_pixels_equal(
        "same guest generation",
        &readback(&device, &queue, &renderer, 2, 2, &full),
        &[0x123456; 4],
        2,
    );
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(101))
        .unwrap();
    assert_pixels_equal(
        "new guest generation",
        &readback(&device, &queue, &renderer, 2, 2, &full),
        &[0xabcdef; 4],
        2,
    );
    renderer
        .upload(&device, &queue, &[0; 4], 2, 2, None)
        .unwrap();
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(101))
        .unwrap();
    assert_pixels_equal(
        "CPU upload resets guest generation",
        &readback(&device, &queue, &renderer, 2, 2, &full),
        &[0xabcdef; 4],
        2,
    );

    // Empty sources and zero on either source axis clear the output. The
    // non-empty 1-pixel-axis cases above exercise valid degenerate textures.
    for (sw, sh) in [(0, 0), (0, 3), (3, 0)] {
        renderer
            .upload(&device, &queue, &[], sw as u32, sh as u32, None)
            .unwrap();
        for scaler in [Scaler::Integer, Scaler::Smooth, Scaler::Area] {
            let (width, height) = (7, 5);
            let view = scaler::viewport(sw, width, height, scaler);
            let actual = readback_with(
                &device,
                &queue,
                &renderer,
                width as u32,
                height as u32,
                &view,
                scaler,
                false,
            );
            assert_pixels_equal(
                &format!("empty {sw}x{sh} source with {scaler:?}"),
                &actual,
                &vec![0; width * height],
                width,
            );
        }
    }

    // Exercise real GPU scaling and overlays against the complete software
    // renderer for every menu/REC visibility combination.
    let source: Vec<u32> = (0..320 * 200).map(|i| (i * 757) & 0xffffff).collect();
    let original = source.clone();
    let (width, height) = (641, 481);
    renderer
        .upload(&device, &queue, &source, 320, 200, Some(500))
        .unwrap();
    for scaler in [Scaler::Integer, Scaler::Smooth, Scaler::Area] {
        for menu_open in [false, true] {
            for recording in [None, Some(61_500)] {
                let mut menu = overlay::Menu::default();
                if menu_open {
                    menu.toggle();
                }
                let values = overlay::Values {
                    scaler,
                    fullscreen: false,
                    mute: false,
                    mute_locked: false,
                    recording,
                    serial: overlay::Link::default(),
                    modem: overlay::Link::default(),
                };
                let layers = overlay::Layers {
                    menu: &menu,
                    values: &values,
                    pulse_ms: &|| 500,
                };
                renderer
                    .update_overlays(
                        &device,
                        &queue,
                        PhysicalSize::new(width as u32, height as u32),
                        &layers,
                    )
                    .unwrap();
                let mut expected = vec![0; width * height];
                Software::render(
                    &mut Blitter::default(),
                    Frame {
                        pixels: &source,
                        width: 320,
                        height: 200,
                        generation: 500,
                    },
                    &mut expected,
                    PhysicalSize::new(width as u32, height as u32),
                    scaler,
                    &layers,
                );
                let view = scaler::viewport(320, width, height, scaler);
                let actual = readback_with(
                    &device,
                    &queue,
                    &renderer,
                    width as u32,
                    height as u32,
                    &view,
                    scaler,
                    true,
                );
                assert_pixels_equal(
                    &format!("GPU overlays with {scaler:?}, menu={menu_open}, REC={recording:?}"),
                    &actual,
                    &expected,
                    width,
                );
            }
        }
    }

    // Menu and REC placements use independent cropped textures. Cover clipped
    // small windows, both letterbox directions, and several overlay scales.
    for (width, height, scaler, pulse) in [
        (160, 120, Scaler::Integer, 0),
        (1200, 600, Scaler::Smooth, 250),
        (600, 1600, Scaler::Area, 500),
        (1920, 1080, Scaler::Smooth, 750),
        (701, 999, Scaler::Area, 250),
    ] {
        let mut menu = overlay::Menu::default();
        menu.toggle();
        let values = overlay::Values {
            scaler,
            fullscreen: true,
            mute: true,
            mute_locked: false,
            recording: Some(125_500),
            serial: overlay::Link::default(),
            modem: overlay::Link::default(),
        };
        let layers = overlay::Layers {
            menu: &menu,
            values: &values,
            pulse_ms: &|| pulse,
        };
        renderer
            .update_overlays(
                &device,
                &queue,
                PhysicalSize::new(width as u32, height as u32),
                &layers,
            )
            .unwrap();
        let mut expected = vec![0; width * height];
        Software::render(
            &mut Blitter::default(),
            Frame {
                pixels: &source,
                width: 320,
                height: 200,
                generation: 500,
            },
            &mut expected,
            PhysicalSize::new(width as u32, height as u32),
            scaler,
            &layers,
        );
        let view = scaler::viewport(320, width, height, scaler);
        let actual = readback_with(
            &device,
            &queue,
            &renderer,
            width as u32,
            height as u32,
            &view,
            scaler,
            true,
        );
        assert_pixels_equal(
            &format!("GPU overlay layout {width}x{height}, {scaler:?}, pulse={pulse}"),
            &actual,
            &expected,
            width,
        );
    }
    assert_eq!(
        source, original,
        "GPU overlays must leave recording source untouched"
    );
    // CRT stays CPU-composed. Upload the software result at window resolution
    // and draw it as a full-screen texture, including both overlay layers.
    for menu_open in [false, true] {
        for recording in [None, Some(61_500)] {
            let mut menu = overlay::Menu::default();
            if menu_open {
                menu.toggle();
            }
            let values = overlay::Values {
                scaler: Scaler::Crt,
                fullscreen: false,
                mute: false,
                mute_locked: false,
                recording,
                serial: overlay::Link::default(),
                modem: overlay::Link::default(),
            };
            let layers = overlay::Layers {
                menu: &menu,
                values: &values,
                pulse_ms: &|| 500,
            };
            let mut expected = vec![0; width * height];
            Software::render(
                &mut Blitter::default(),
                Frame {
                    pixels: &source,
                    width: 320,
                    height: 200,
                    generation: 500,
                },
                &mut expected,
                PhysicalSize::new(width as u32, height as u32),
                Scaler::Crt,
                &layers,
            );
            renderer
                .upload(
                    &device,
                    &queue,
                    &expected,
                    width as u32,
                    height as u32,
                    None,
                )
                .unwrap();
            let full = Viewport {
                x: 0,
                y: 0,
                width,
                height,
            };
            let actual = readback_with(
                &device,
                &queue,
                &renderer,
                width as u32,
                height as u32,
                &full,
                Scaler::Crt,
                false,
            );
            assert_pixels_equal(
                &format!("CPU-composited CRT, menu={menu_open}, REC={recording:?}"),
                &actual,
                &expected,
                width,
            );
        }
    }
}

fn assert_pixels_equal(label: &str, actual: &[u32], expected: &[u32], width: usize) {
    if let Some(index) = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected)
    {
        panic!(
            "{label}: first mismatch at ({},{}): GPU=0x{:06x}, Software=0x{:06x}",
            index % width,
            index / width,
            actual[index],
            expected[index]
        );
    }
    assert_eq!(actual.len(), expected.len(), "{label}: pixel count differs");
}

fn overlay_upload_counts(renderer: &Renderer) -> (usize, usize) {
    (renderer.menu.uploads, renderer.recording.uploads)
}

fn assert_upload_delta(
    renderer: &Renderer,
    before: (usize, usize),
    menu: usize,
    recording: usize,
    label: &str,
) {
    assert_eq!(
        overlay_upload_counts(renderer),
        (before.0 + menu, before.1 + recording),
        "{label}"
    );
}

fn update_overlay_cache(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: PhysicalSize<u32>,
    menu: &overlay::Menu,
    values: &overlay::Values,
    pulse_ms: u64,
) {
    let layers = overlay::Layers {
        menu,
        values,
        pulse_ms: &|| pulse_ms,
    };
    renderer
        .update_overlays(device, queue, size, &layers)
        .unwrap();
}

#[test]
fn overlay_textures_upload_only_when_their_pixels_change() {
    let Some((device, queue)) = offscreen_device("overlay upload counters") else {
        return;
    };
    let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    let mut menu = overlay::Menu::default();
    let mut values = overlay::Values {
        scaler: Scaler::Integer,
        fullscreen: false,
        mute: false,
        mute_locked: false,
        recording: None,
        serial: overlay::Link::default(),
        modem: overlay::Link::default(),
    };
    let small = PhysicalSize::new(640, 480);

    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 0);
    assert_upload_delta(&renderer, before, 0, 0, "initially stopped overlays");
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 0);
    assert_upload_delta(&renderer, before, 0, 0, "unchanged stopped overlays");

    // Guest uploads and generation changes do not invalidate host textures.
    renderer
        .upload(&device, &queue, &[0x123456; 4], 2, 2, Some(10))
        .unwrap();
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(11))
        .unwrap();
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 0);
    assert_upload_delta(&renderer, before, 0, 0, "guest generation changed");

    values.recording = Some(61_500);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 500);
    assert_upload_delta(&renderer, before, 0, 1, "recording starts");
    // The displayed second and the rounded pulse red are unchanged.
    values.recording = Some(61_501);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1501);
    assert_upload_delta(
        &renderer,
        before,
        0,
        0,
        "same clock second and pulse colour",
    );
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 0);
    assert_upload_delta(&renderer, before, 0, 1, "pulse red changes");
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 0, "pulse rounds to the same red");
    values.recording = Some(62_500);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 1, "recording clock text changes");

    menu.toggle();
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "menu opens");
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 0, "same open menu");

    menu.handle(overlay::Key::Down, &values);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "menu selection moves");
    values.mute_locked = true;
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "mute lock text changes");
    values.serial = overlay::Link {
        enabled: true,
        status: gp64_machine::modem::LineStatus::Connected("10.0.0.2:4000".into()),
        peer: Some("desk".into()),
        com: 1,
    };
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "link value appears");
    values.serial.peer = Some("east desk".into());
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, small, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "link text changes");

    // These dimensions retain the same 4:3 crop, but both cropped layers need
    // new pixels and placements for the larger host window.
    let large = PhysicalSize::new(800, 600);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(
        &renderer,
        before,
        1,
        1,
        "window resizes at the same aspect ratio",
    );

    menu.handle(overlay::Key::Up, &values);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(
        &renderer,
        before,
        1,
        0,
        "selection returns to the first row",
    );

    // The closed menu and stopped REC keep their cached textures for reuse.
    menu.toggle();
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 0, "menu closes");
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 0, "menu remains closed");
    menu.toggle();
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(
        &renderer,
        before,
        0,
        0,
        "menu reopens with cached selection and text",
    );

    values.recording = None;
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(
        &renderer,
        before,
        1,
        0,
        "recording stops and menu value changes",
    );
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 0, 0, "recording remains stopped");
    values.recording = Some(62_500);
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "recording resumes in the menu");

    values.scaler = Scaler::Smooth;
    let before = overlay_upload_counts(&renderer);
    update_overlay_cache(&mut renderer, &device, &queue, large, &menu, &values, 1);
    assert_upload_delta(&renderer, before, 1, 0, "menu scaler label changes");
}
