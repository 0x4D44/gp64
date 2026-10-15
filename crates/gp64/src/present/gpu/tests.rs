use super::*;

#[test]
fn wgsl_parses_and_validates() {
    let module = wgpu::naga::front::wgsl::parse_str(SHADER).unwrap();
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap();
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

fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    width: u32,
    height: u32,
    view: &Viewport,
) -> Vec<u32> {
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
            send.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })
        .unwrap();
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
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

#[test]
fn offscreen_integer_and_composited_pixels_match_cpu() {
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
            eprintln!("SKIP offscreen GPU comparison: no headless adapter: {error}");
            return;
        }
    };
    eprintln!(
        "Offscreen GPU comparison running on {:?}",
        adapter.get_info()
    );
    let (device, queue) = block_on(adapter.request_device(&Default::default()))
        .expect("available adapter must create a device");
    let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    let mut generation = 0;
    for (sw, sh) in [(1, 1), (7, 5), (319, 199), (320, 200), (640, 350)] {
        let source: Vec<u32> = (0..sw * sh)
            .map(|i| (i as u32 * 757 + 0x123456) & 0xffffff)
            .collect();
        generation += 1;
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
        for (width, height) in [(1, 1), (2, 3), (13, 7), (79, 61), (641, 481), (997, 731)] {
            let view = scaler::viewport(sw, width, height, Scaler::Integer);
            let actual = readback(
                &device,
                &queue,
                &renderer,
                width as u32,
                height as u32,
                &view,
            );
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
            assert!(
                actual == expected,
                "GPU Integer mismatch: {sw}x{sh} -> {width}x{height}"
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
    assert_eq!(
        readback(&device, &queue, &renderer, 2, 2, &full),
        vec![0x123456; 4]
    );
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(101))
        .unwrap();
    assert_eq!(
        readback(&device, &queue, &renderer, 2, 2, &full),
        vec![0xabcdef; 4]
    );
    renderer
        .upload(&device, &queue, &[0; 4], 2, 2, None)
        .unwrap();
    renderer
        .upload(&device, &queue, &[0xabcdef; 4], 2, 2, Some(101))
        .unwrap();
    assert_eq!(
        readback(&device, &queue, &renderer, 2, 2, &full),
        vec![0xabcdef; 4]
    );

    let source: Vec<u32> = (0..320 * 200).map(|i| (i * 757) & 0xffffff).collect();
    let original = source.clone();
    for scaler in Scaler::ALL {
        for menu_open in [false, true] {
            let mut menu = overlay::Menu::default();
            if menu_open {
                menu.toggle();
            }
            let values = overlay::Values {
                scaler,
                fullscreen: false,
                mute: false,
                mute_locked: false,
                recording: Some(61500),
                serial: overlay::Link::default(),
                modem: overlay::Link::default(),
            };
            let mut expected = vec![0; 641 * 481];
            Software::render(
                &mut Blitter::default(),
                Frame {
                    pixels: &source,
                    width: 320,
                    height: 200,
                    generation: 200,
                },
                &mut expected,
                PhysicalSize::new(641, 481),
                scaler,
                &overlay::Layers {
                    menu: &menu,
                    values: &values,
                    pulse_ms: &|| 500,
                },
            );
            renderer
                .upload(&device, &queue, &expected, 641, 481, None)
                .unwrap();
            let view = Viewport {
                x: 0,
                y: 0,
                width: 641,
                height: 481,
            };
            assert!(
                readback(&device, &queue, &renderer, 641, 481, &view) == expected,
                "composited {scaler:?}, menu={menu_open}"
            );
        }
    }
    assert_eq!(
        source, original,
        "GPU overlays must leave recording source untouched"
    );
    renderer
        .upload(&device, &queue, &[], 0, 0, Some(300))
        .unwrap();
    assert_eq!(
        readback(&device, &queue, &renderer, 2, 2, &full),
        vec![0; 4]
    );
}
