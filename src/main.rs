mod app;
mod art;
mod cava;
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
            .with_module_level("mpd_display_rs", LevelFilter::Info),
    };
    if let Err(e) = logger.init() {
        eprintln!("failed to initialise logger: {e}");
    }

    let config = Config::from_env();
    info!(
        "MPD at {}:{}, layout {:?}, lyrics {}",
        config.mpd_host,
        config.mpd_port,
        config.layout,
        if config.lyrics_enabled { "on" } else { "off" },
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MPD Display")
            .with_app_id("mpd-display-rs")
            .with_inner_size([1280.0, 720.0])
            .with_fullscreen(true),
        ..Default::default()
    };
    let result = eframe::run_native(
        "MPD Display",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, config)))),
    );
    if let Err(e) = result {
        error!("failed to run window: {e}");
        std::process::exit(1);
    }
}
