use crate::config::Config;
use crate::input::KeyboardEmulator;
use crate::skillcheck_logic::{
    Circle, Pixel, SkillCheckParams, SkillCheckState, generate_patterns, process_skillcheck_frame,
};
use tokio::sync::mpsc::Sender;
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

pub struct CaptureFlags {
    pub config: Config,
    pub log_tx: Sender<String>,
}

struct CaptureHandler {
    log_tx: Sender<String>,
    patterns: (Vec<Pixel>, Vec<Pixel>, Vec<Pixel>),
    state: SkillCheckState,
    params: SkillCheckParams,
    crop_size: u32,
    circle_center_x: u32,
    circle_center_y: u32,
    input_emulator: KeyboardEmulator,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let flags = ctx.flags;
        let crop = flags.config.geometry.crop_size;
        let radius = flags.config.geometry.circle_radius;
        let params = SkillCheckParams::from(&flags.config);

        let input_emulator = KeyboardEmulator::new(
            &flags.config.input.device_name,
            flags.config.input.vendor_id,
            flags.config.input.product_id,
        )?;

        let patterns = generate_patterns(&Circle {
            center: Pixel {
                x: crop / 2,
                y: crop / 2,
            },
            radius,
            diameter: radius * 2,
        });

        Ok(Self {
            log_tx: flags.log_tx,
            patterns,
            state: SkillCheckState::InSearch,
            params,
            crop_size: crop,
            circle_center_x: flags.config.geometry.circle_center_x,
            circle_center_y: flags.config.geometry.circle_center_y,
            input_emulator,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();
        let mut buffer_guard = frame.buffer()?;
        let buffer = buffer_guard.as_raw_buffer();

        let crop = self.crop_size;
        let crop_half = crop / 2;
        let start_x = self.circle_center_x as i32 - crop_half as i32;
        let start_y = self.circle_center_y as i32 - crop_half as i32;

        let mut cropped_pixels = vec![0u8; (crop * crop * 4) as usize];

        for y in 0..crop {
            let src_y = start_y + y as i32;
            if src_y < 0 || src_y >= height as i32 {
                continue;
            }
            let src_row_start = src_y as usize * width as usize * 4;
            for x in 0..crop {
                let src_x = start_x + x as i32;
                if src_x < 0 || src_x >= width as i32 {
                    continue;
                }
                let src_idx = src_row_start + src_x as usize * 4;
                let dst_idx = (y as usize * crop as usize + x as usize) * 4;
                if src_idx + 3 < buffer.len() {
                    // Copy BGRA bytes directly
                    cropped_pixels[dst_idx] = buffer[src_idx]; // B
                    cropped_pixels[dst_idx + 1] = buffer[src_idx + 1]; // G
                    cropped_pixels[dst_idx + 2] = buffer[src_idx + 2]; // R
                    cropped_pixels[dst_idx + 3] = buffer[src_idx + 3]; // A
                }
            }
        }

        process_skillcheck_frame(
            &cropped_pixels,
            (crop * 4) as usize,
            &self.patterns,
            &mut self.state,
            &self.params,
            &mut self.input_emulator,
            &self.log_tx,
        );

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.log_tx
            .try_send("Capture session closed".to_string())
            .ok();
        Ok(())
    }
}

pub async fn run_bot(
    cfg: Config,
    rx: tokio::sync::oneshot::Receiver<()>,
    log_tx: Sender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    log_tx
        .try_send("Searching for game window...".to_string())
        .ok();

    let window = match Window::from_contains_name("DeadByDaylight") {
        Ok(w) => w,
        Err(e) => {
            log_tx
                .try_send(format!("Failed to find game window: {:?}", e))
                .ok();
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Game window not found",
            )));
        }
    };

    let title = window.title().unwrap_or_else(|_| "Unknown".to_string());
    log_tx
        .try_send(format!("Found window: '{}'. Starting capture...", title))
        .ok();

    let settings = Settings::new(
        window,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        CaptureFlags {
            config: cfg,
            log_tx: log_tx.clone(),
        },
    );

    // Start free-threaded (does not block current thread)
    let capture_control = CaptureHandler::start_free_threaded(settings)?;

    // Wait for the stop signal from GUI
    let _ = rx.await;

    log_tx
        .try_send("Stopping Windows capture session...".to_string())
        .ok();

    // Stop the session gracefully
    if let Err(e) = capture_control.stop() {
        log_tx
            .try_send(format!("Error during capture stop: {:?}", e))
            .ok();
    }

    log_tx.try_send("Bot stopped gracefully".to_string()).ok();

    Ok(())
}
