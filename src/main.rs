mod app;
mod art;
mod config;
mod export;
mod player;
mod ui;

use log::{error, info, LevelFilter};

use crate::config::Config;

fn main() {
    // RUST_LOG overrides the default of info for this crate and warnings for dependencies.
    let logger = match std::env::var_os("RUST_LOG") {
        Some(_) => simple_logger::SimpleLogger::new().env(),
        None => simple_logger::SimpleLogger::new()
            .with_level(LevelFilter::Warn)
            .with_module_level("encore", LevelFilter::Info),
    };
    if let Err(e) = logger.init() {
        eprintln!("failed to initialise logger: {e}");
    }

    config::write_starter_config();
    let config = Config::from_env();
    info!(
        "source {:?}, MPD at {}:{}, layout {:?}, lyrics {}",
        config.source,
        config.mpd_host,
        config.mpd_port,
        config.layout,
        if config.lyrics_enabled { "on" } else { "off" },
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Encore")
            .with_app_id("encore")
            .with_inner_size([1280.0, 720.0])
            .with_fullscreen(true),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Encore",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, config)))),
    );
    if let Err(e) = result {
        error!("failed to run window: {e}");
        std::process::exit(1);
    }
}
