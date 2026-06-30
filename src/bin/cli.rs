use dbd_auto_skillcheck::config::get_config;

#[tokio::main]
async fn main() {
    let config = get_config();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel::<String>(100);
    if let Err(e) = dbd_auto_skillcheck::platform::run_bot(config.clone(), rx, log_tx).await {
        eprintln!("Bot engine error: {}", e);
    }
    while let Some(log) = log_rx.recv().await {
        println!("{}", log);
    }
}
