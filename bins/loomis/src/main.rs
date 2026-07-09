//! Loomis — Modular Agent with TUI.
//!
//! Entry point for the Loomis binary.

use std::path::PathBuf;

use tools::SandboxConfig;

const DEFAULT_MODEL: &str = "deepseek-v4-pro";
const DEFAULT_FLASH_MODEL: &str = "deepseek-v4-flash";

#[tokio::main]
async fn main() {
    let use_tui = !std::env::args().any(|a| a == "--no-tui");

    // Load environment
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        eprintln!("ERROR: DEEPSEEK_API not set.");
        eprintln!("Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let model = std::env::var("DEFAULT_PRO_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let flash_model =
        std::env::var("FLASH_MODEL").unwrap_or_else(|_| DEFAULT_FLASH_MODEL.to_string());

    // Load sandbox config from .loomis/config.toml (falls back to safe defaults).
    let sandbox_config = match SandboxConfig::load(&cwd) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("WARNING: Failed to load sandbox config: {e}");
            eprintln!("Using safe defaults.");
            SandboxConfig::default()
        }
    };

    let kit = loomis::build_coding_agent(&api_key, &cwd, &model, &flash_model, &sandbox_config);

    if use_tui {
        match loomis::tui::run(
            kit.agent,
            kit.memory,
            kit.tool_names,
            &kit.model,
            cwd,
            kit.agent_rx,
            kit.agent_tx,
            kit.approval_tx,
        ) {
            Ok(()) => {}
            Err(e) => eprintln!("TUI error: {e}"),
        }
    } else {
        eprintln!("--no-tui mode is not yet migrated to the new crate structure.");
        eprintln!("Please use the TUI mode for now.");
        std::process::exit(1);
    }
}
