use crate::config::Config;
use crate::input::KeyboardEmulator;
use crate::platform::vulkan_linux::VulkanDmaBufBackend;
use crate::skillcheck_logic::{
    Circle, Pixel, SkillCheckParams, SkillCheckState, generate_patterns, process_skillcheck_frame,
};
use ashpd::desktop::{
    PersistMode,
    screencast::{
        CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream as ScreencastStream,
    },
};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::os::fd::{AsFd, OwnedFd};

struct UserData {
    log_tx: tokio::sync::mpsc::Sender<String>,
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

async fn start_streaming(
    node_id: u32,
    fd: OwnedFd,
    cfg: &Config,
    rx: tokio::sync::oneshot::Receiver<()>,
    log_tx: tokio::sync::mpsc::Sender<String>,
) -> Result<(), pw::Error> {
    log_tx
        .try_send(format!(
            "Config loaded: latency_ms={}, ring_threshold={}, inner_enter={}",
            cfg.timing.latency_ms, cfg.detection.ring_threshold, cfg.detection.inner_enter
        ))
        .ok();

    let crop = cfg.geometry.crop_size;
    let crop_offset_x = cfg.geometry.circle_center_x as i32 - crop as i32 / 2;
    let crop_offset_y = cfg.geometry.circle_center_y as i32 - crop as i32 / 2;
    let radius = cfg.geometry.circle_radius;

    let params = SkillCheckParams::from(cfg);

    let input_emulator = KeyboardEmulator::new(
        &cfg.input.device_name,
        cfg.input.vendor_id,
        cfg.input.product_id,
    )
    .expect("Failed to init input emulator");

    pw::init();

    // let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(fd, None)?;
    //
    let data = UserData {
        log_tx: log_tx.clone(),
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
    let (pw_signal_tx, pw_signal_rx) = pw::channel::channel::<()>();
    let _receiver = pw_signal_rx.attach(&mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| {
            mainloop.quit();
        }
    });

    let local_rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        local_rt.block_on(async move {
            let _ = rx.await;
        });
        let _ = pw_signal_tx.send(());
    });

    // pw mainloop listener with callbacks
    // todo
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed({
            let log_tx = log_tx.clone();
            move |_, _, old, new| {
                log_tx
                    .try_send(format!("State changed: {:?} -> {:?}", old, new))
                    .ok();
            }
        })
        .param_changed({
            let log_tx = log_tx.clone();
            move |_, user_data, id, param| {
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

                let fmt_msg = format!(
                    "Got video format: {}x{} @ {}/{} fps, format: {:?}",
                    user_data.format.size().width,
                    user_data.format.size().height,
                    user_data.format.framerate().num,
                    user_data.format.framerate().denom,
                    user_data.format.format()
                );
                log_tx.try_send(fmt_msg).ok();
            }
        })
        .process({
            let log_tx = log_tx.clone();
            move |stream, user_data| match stream.dequeue_buffer() {
                None => {
                    log_tx.try_send("Out of buffers".to_string()).ok();
                }
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
                    match vk.capture_crop(pw_fd, width, height, modifier, stride) {
                        Ok(pixels) => {
                            process_skillcheck_frame(
                                pixels,
                                (user_data.crop_size * 4) as usize,
                                &user_data.patternes,
                                &mut user_data.state,
                                &user_data.params,
                                &mut user_data.input_emulator,
                                &user_data.log_tx,
                            );
                        }
                        Err(e) => {
                            log_tx
                                .try_send(format!("capture_crop failed: {:?}", e))
                                .ok();
                        }
                    }
                }
            }
        })
        .register()?;
    log_tx
        .try_send(format!("Created stream: {:?}", stream.name()))
        .ok();
    log_tx
        .try_send(format!("Created stream {:#?}", stream))
        .ok();
    let mut params = [];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    log_tx
        .try_send("Connected stream to PipeWire".to_string())
        .ok();
    log_tx.try_send("Connected stream".to_string()).ok();

    mainloop.run();

    Ok(())
}

pub async fn run_bot(
    cfg: Config,
    rx: tokio::sync::oneshot::Receiver<()>,
    log_tx: tokio::sync::mpsc::Sender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    log_tx.try_send("Opening portal...".to_string()).ok();
    let (stream, fd) = open_portal().await?;
    let pipewire_node_id = stream.pipe_wire_node_id();

    let node_msg = format!(
        "Portal opened. PipeWire node ID: {}, FD: {:?}",
        pipewire_node_id,
        fd.as_fd()
    );
    log_tx.try_send(node_msg).ok();

    start_streaming(pipewire_node_id, fd, &cfg, rx, log_tx.clone()).await?;
    log_tx.try_send("Bot stopped gracefully".to_string()).ok();
    Ok(())
}
