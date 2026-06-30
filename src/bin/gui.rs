use dbd_auto_skillcheck::config::get_config;
#[tokio::main]
async fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_min_inner_size([500.0, 400.0]),
        ..Default::default()
    };
    let config = get_config();
    eframe::run_native(
        "DBD auto skillcheck",
        native_options,
        Box::new(|cc| {
            Ok(Box::new(dbd_auto_skillcheck::AutoSkillCheck::new(
                cc, config,
            )))
        }),
    )
}
