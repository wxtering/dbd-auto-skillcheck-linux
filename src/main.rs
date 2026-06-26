use ashpd::desktop::{
    PersistMode,
    screencast::{
        CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream as ScreencastStream,
    },
};
mod config;
mod input;
mod skillcheck_logic;
mod vulkan;
use config::get_config;
use input::KeyboardEmulator;
use pipewire as pw;
use pw::{properties::properties, spa};
use skillcheck_logic::{
    Circle, Pixel, SkillCheckParams, SkillCheckState, generate_patterns, process_skillcheck_frame,
};
use std::os::fd::{AsFd, OwnedFd};
use vulkan::VulkanDmaBufBackend;

struct UserData {
    format: spa::param::video::VideoInfoRaw,
    vk_backend: Option<VulkanDmaBufBackend>,
    patternes: (Vec<Pixel>, Vec<Pixel>, Vec<Pixel>),
    state: SkillCheckState,
    params: SkillCheckParams,
    crop_size: u32,
    input_emulator: KeyboardEmulator,
}

async fn open_portal() -> ashpd::Result<(ScreencastStream, OwnedFd)> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Hidden)
                .set_sources(SourceType::Monitor | SourceType::Window)
                .set_multiple(false)
                .set_restore_token(None)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await?;

    let response = proxy
        .start(&session, None, Default::default())
        .await?
        .response()?;
    let stream = response
        .streams()
        .first()
        .expect("no stream found / selected")
        .to_owned();

    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await?;

    Ok((stream, fd))
}

async fn start_streaming(node_id: u32, fd: OwnedFd) -> Result<(), pw::Error> {
    // Load config (creates default if missing).
    let cfg = get_config();
    println!(
        "Config loaded: latency_ms={}, ring_boost={}, inner_enter={}",
        cfg.timing.latency_ms, cfg.detection.ring_boost, cfg.detection.inner_enter
    );

    let crop = cfg.geometry.crop_size;
    let crop_offset_x = cfg.geometry.circle_center_x as i32 - crop as i32 / 2;
    let crop_offset_y = cfg.geometry.circle_center_y as i32 - crop as i32 / 2;
    let radius = cfg.geometry.circle_radius;

    // Build SkillCheckParams for the logic layer.
    let params = SkillCheckParams::from(&cfg);

    // Init input emulator with config values.
    let input_emulator = KeyboardEmulator::new(
        &cfg.input.device_name,
        cfg.input.vendor_id,
        cfg.input.product_id,
    )
    .expect("Failed to init input emulator");

    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(fd, None)?;

    let data = UserData {
        format: Default::default(),
        vk_backend: Some(
            VulkanDmaBufBackend::new(crop_offset_x, crop_offset_y, crop, crop)
                .expect("Failed to init Vulkan"),
        ),
        patternes: (Vec::new(), Vec::new(), Vec::new()),
        state: SkillCheckState::InSearch,
        params,
        crop_size: crop,
        input_emulator,
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "dbd-auto-skillcheck",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, _, old, new| {
            println!("State changed: {:?} -> {:?}", old, new);
        })
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) =
                match pw::spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };

            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            // Patterns in CROP-relative coords: circle center = crop center.
            user_data.patternes = generate_patterns(&Circle {
                center: Pixel {
                    x: user_data.crop_size / 2,
                    y: user_data.crop_size / 2,
                },
                radius,
                diameter: radius * 2,
            });
            user_data
                .format
                .parse(param)
                .expect("Failed to parse param changed to VideoInfoRaw");

            println!("got video format:");
            println!(
                "\tformat: {} ({:?})",
                user_data.format.format().as_raw(),
                user_data.format.format()
            );
            println!(
                "\tsize: {}x{}",
                user_data.format.size().width,
                user_data.format.size().height
            );
            println!(
                "\tframerate: {}/{}",
                user_data.format.framerate().num,
                user_data.format.framerate().denom
            );
        })
        .process(|stream, user_data| match stream.dequeue_buffer() {
            None => println!("out of buffers"),
            Some(mut buffer) => {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let pw_fd = data.fd();
                let stride = data.chunk().stride() as u32;
                let modifier = user_data.format.modifier();
                let width = user_data.format.size().width;
                let height = user_data.format.size().height;

                let Some(vk) = user_data.vk_backend.as_mut() else {
                    return;
                };
                // Single call: import frame + crop to CPU → mapped pixels.
                match vk.capture_crop(pw_fd, width, height, modifier, stride) {
                    Ok(pixels) => {
                        process_skillcheck_frame(
                            pixels,
                            (user_data.crop_size * 4) as usize,
                            &user_data.patternes,
                            &mut user_data.state,
                            &user_data.params,
                            &mut user_data.input_emulator,
                        );
                    }
                    Err(e) => println!("capture_crop failed: {:?}", e),
                }
            }
        })
        .register()?;
    println!("Created stream {:#?}", stream);
    let mut params = [];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    println!("Connected stream");

    mainloop.run();

    Ok(())
}

#[tokio::main]
async fn main() {
    let (stream, fd) = open_portal().await.expect("failed to open portal");
    let pipewire_node_id = stream.pipe_wire_node_id();

    println!("node id {}, fd {:?}", pipewire_node_id, fd.as_fd());

    if let Err(e) = start_streaming(pipewire_node_id, fd).await {
        eprintln!("Error: {}", e);
    };
}
