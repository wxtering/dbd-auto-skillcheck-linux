use egui::{CentralPanel, MenuBar, Panel, ScrollArea, ViewportCommand};

use crate::{bot::run_bot, config::Config};

pub struct AutoSkillCheck {
    config: Config,
    thread_handler: Option<tokio::task::JoinHandle<()>>,
    pw_tx: Option<pipewire::channel::Sender<()>>,
    log_tx: tokio::sync::mpsc::Sender<String>,
    pw_log_rx: tokio::sync::mpsc::Receiver<String>,
    logs: std::collections::VecDeque<String>,
}

impl Default for AutoSkillCheck {
    fn default() -> Self {
        let (log_tx, log_rx) = tokio::sync::mpsc::channel(100);
        Self {
            config: Config::default(),
            thread_handler: None,
            pw_tx: None,
            log_tx,
            pw_log_rx: log_rx,
            logs: std::collections::VecDeque::new(),
        }
    }
}

impl AutoSkillCheck {
    /// Called once before the first frame.
    pub fn new(_cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        let mut app = Self::default();
        app.config = config;
        app
    }
    pub fn log(&mut self, message: impl Into<String>) {
        self.logs.push_back(message.into());
        if self.logs.len() > 200 {
            self.logs.pop_front();
        }
    }
}
impl Drop for AutoSkillCheck {
    fn drop(&mut self) {
        if let Some(tx) = self.pw_tx.take() {
            let _ = tx.send(());
        }
    }
}
impl eframe::App for AutoSkillCheck {
    /// Called each time the UI needs repainting, which may be many times per second.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        Panel::top("top_panel").show(ui, |ui| {
            MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ui.send_viewport_cmd(ViewportCommand::Close);
                    }
                });
                ui.add_space(16.0);
                egui::widgets::global_theme_preference_buttons(ui);
            });
        });

        Panel::left("left_panel").show(ui, |ui| {
            ui.heading("auto skillcheck gui");
            render_config_block(ui, self);
            render_log_block(ui, self);
        });

        // Check if thread panicked or finished naturally to reset UI state
        if let Some(h) = &self.thread_handler {
            if h.is_finished() {
                self.thread_handler = None;
                self.pw_tx = None;
                self.log("Bot thread exited or stopped.");
            }
        }

        CentralPanel::default().show(ui, |ui| {
            if self.thread_handler.is_some() {
                if ui.button("Stop").clicked() {
                    self.log("Stopping bot...");
                    if let Some(tx) = self.pw_tx.take() {
                        let _ = tx.send(());
                    }
                }
            } else {
                if ui.button("Start").clicked() {
                    self.log("Starting bot...");
                    self.logs.clear(); // Clear logs on start
                    let config = self.config.clone();
                    let (tx, rx) = pipewire::channel::channel::<()>();
                    let log_tx = self.log_tx.clone();
                    self.pw_tx = Some(tx);

                    let h = tokio::spawn(async move {
                        let err_msg = run_bot(config, rx, log_tx.clone())
                            .await
                            .err()
                            .map(|e| format!("Bot error: {}", e));
                        if let Some(msg) = err_msg {
                            let _ = log_tx.send(msg).await;
                        }
                    });
                    self.thread_handler = Some(h);
                }
            }
        });
    }
}

/// this fn represents the configuration block
fn render_config_block(ui: &mut egui::Ui, object: &mut AutoSkillCheck) {
    ui.group(|ui| {
        ui.heading("Config");
        ui.set_max_width(300.0);

        egui::Grid::new("primary_config_grid")
            .num_columns(2)
            .spacing([40.0, 8.0])
            .show(ui, |ui| {
                ui.label("Latency (ms):");
                ui.add(
                    egui::DragValue::new(&mut object.config.timing.latency_ms)
                        .speed(1.0)
                        .range(-100.0..=500.0),
                );
                ui.end_row();

                ui.label("Circle Radius:");
                ui.add(
                    egui::DragValue::new(&mut object.config.geometry.circle_radius)
                        .speed(1.0)
                        .range(10..=500),
                );
                ui.end_row();

                ui.label("Center X:");
                ui.add(
                    egui::DragValue::new(&mut object.config.geometry.circle_center_x)
                        .speed(1.0)
                        .range(0..=7680),
                );
                ui.end_row();

                ui.label("Center Y:");
                ui.add(
                    egui::DragValue::new(&mut object.config.geometry.circle_center_y)
                        .speed(1.0)
                        .range(0..=4320),
                );
                ui.end_row();

                ui.label("Ring Threshold:");
                ui.add(
                    egui::DragValue::new(&mut object.config.detection.ring_threshold)
                        .speed(0.01)
                        .range(0.0..=1.0),
                );
                ui.end_row();

                ui.label("Dark Threshold:");
                ui.add(
                    egui::DragValue::new(&mut object.config.colors.dark_val)
                        .speed(0.01)
                        .range(0.0..=1.0),
                );
            });

        ui.add_space(8.0);
        ui.collapsing("Advanced Settings", |ui| {
            ui.add_space(4.0);
            ui.collapsing("Input Settings", |ui| {
                egui::Grid::new("input_config_grid")
                    .num_columns(2)
                    .spacing([20.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Device:");
                        ui.text_edit_singleline(&mut object.config.input.device_name);
                        ui.end_row();

                        ui.label("Vendor ID:");
                        ui.add(egui::DragValue::new(&mut object.config.input.vendor_id));
                        ui.end_row();

                        ui.label("Product ID:");
                        ui.add(egui::DragValue::new(&mut object.config.input.product_id));
                        ui.end_row();
                    });
            });

            ui.collapsing("Detection & Timing", |ui| {
                egui::Grid::new("det_timing_config_grid")
                    .num_columns(2)
                    .spacing([20.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Crop Size:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.geometry.crop_size)
                                .range(50..=2000),
                        );
                        ui.end_row();

                        ui.label("Inner Enter:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.detection.inner_enter)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Inner Exit:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.detection.inner_exit)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Ring Discount:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.detection.ring_discount)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Speed History Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.timing.speed_history_min)
                                .range(1..=100),
                        );
                        ui.end_row();

                        ui.label("Calib Samples:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.timing.calibrating_samples)
                                .range(1..=100),
                        );
                        ui.end_row();

                        ui.label("Active Miss:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.timing.active_miss)
                                .range(1..=100),
                        );
                        ui.end_row();

                        ui.label("Calib Miss:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.timing.calibrating_miss)
                                .range(1..=100),
                        );
                        ui.end_row();
                    });
            });

            ui.collapsing("Color Calibration", |ui| {
                egui::Grid::new("color_config_grid")
                    .num_columns(2)
                    .spacing([20.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Red Hue Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.red_hue_min)
                                .speed(1.0)
                                .range(0.0..=360.0),
                        );
                        ui.end_row();

                        ui.label("Red Hue Max:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.red_hue_max)
                                .speed(1.0)
                                .range(0.0..=360.0),
                        );
                        ui.end_row();

                        ui.label("Red Sat Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.red_sat_min)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Red Val Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.red_val_min)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("White Sat Max:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.white_sat_max)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("White Val Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.white_val_min)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Grey Val Min:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.grey_val_min)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Grey Val Max:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.grey_val_max)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();

                        ui.label("Grey Sat Max:");
                        ui.add(
                            egui::DragValue::new(&mut object.config.colors.grey_sat_max)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                        ui.end_row();
                    });
            });
        });

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                if ui.button("Restore defaults").clicked() {
                    object.config = Config::default();
                    if let Err(e) = object.config.save() {
                        object.log(format!("Failed to save config: {}", e));
                    } else {
                        object.log("Config restored to defaults");
                    }
                }
                if ui.button("Save config").clicked() {
                    if let Err(e) = object.config.save() {
                        object.log(format!("Failed to save config: {}", e));
                    } else {
                        object.log("Config saved successfully");
                    }
                }
            });
        });
    });
}
/// this fn represents the log block
fn render_log_block(ui: &mut egui::Ui, object: &mut AutoSkillCheck) {
    while let Ok(message) = object.pw_log_rx.try_recv() {
        object.log(message);
    }

    ui.group(|ui| {
        ui.set_max_width(300.0);
        ui.heading("Logs");
        ui.separator();
        ui.add_space(8.0);

        ScrollArea::vertical()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .stick_to_bottom(true)
            .max_height(300.0)
            .show(ui, |ui| {
                for text in &object.logs {
                    ui.label(text);
                }
            });
        ui.request_repaint();
    });
}
