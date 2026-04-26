#![allow(dead_code)]

mod memory;
mod tui;
mod llm;
mod tools;
mod channels;
mod config;
mod server;
mod cli;

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::runtime::Runtime;

fn main() {
    // ── CLI dispatch ──────────────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    let sub1 = args.get(1).map(|s| s.as_str());
    let sub2 = args.get(2).map(|s| s.as_str());
    let sub3 = args.get(3).map(|s| s.as_str());

    match (sub1, sub2) {
        (Some("server"), Some("start"))   => { cli::server_start(); return; }
        (Some("server"), Some("stop"))    => { cli::server_stop();  return; }
        (Some("server"), Some("status"))  => { cli::server_status(); return; }
        (Some("server"), Some("install")) => { cli::systemd_print(); return; }
        (Some("pairing"), Some("list"))   => { cli::pairing_list(sub3); return; }
        (Some("pairing"), Some("approve"))=> {
            if let Some(user) = sub3 { cli::pairing_approve(user); } else { eprintln!("Usage: omni pairing approve <username>"); }
            return;
        }
        (Some("pairing"), Some("deny"))   => {
            if let Some(user) = sub3 { cli::pairing_deny(user); } else { eprintln!("Usage: omni pairing deny <username>"); }
            return;
        }
        (Some("help") | Some("--help") | Some("-h"), _) => { cli::print_help(); return; }
        _ => {}  // fall through → launch TUI
    }

    // ── TUI mode ──────────────────────────────────────────────────────────────
    config::migrate_old_config();
    let secrets = config::Secrets::load();
    let _ = std::fs::create_dir_all(config::WORKSPACE);

    if config::AppConfig::load().is_none() {
        match tui::app::run_config_wizard() {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Configuration wizard error: {}", e);
                eprintln!("  {{\"ollama_base\":\"http://localhost:11434\",\"model_name\":\"llama3.2\"}}");
                return;
            }
        }
    }

    let cfg = match config::AppConfig::load() {
        Some(c) => c,
        None => { eprintln!("Could not load config.json after setup. Exiting."); return; }
    };

    let db = match memory::database::Database::new("omniscientia.db") {
        Ok(d) => d,
        Err(e) => { eprintln!("DB error: {}", e); return; }
    };
    let user_id = db.upsert_user("Admin", 4).unwrap_or(1);

    // One shared runtime for the whole process
    let rt = Arc::new(
        Runtime::new().expect("Failed to create Tokio runtime")
    );

    // Start background bot server inside the runtime
    let (bot_tx, bot_rx) = mpsc::channel::<server::BotEvent>(64);
    let bot_srv = server::BotServer::new(secrets, "omniscientia.db".to_string(), bot_tx);
    let _guard = rt.enter();
    bot_srv.start();
    drop(_guard);

    let mut app = tui::app::App::new("Admin".to_string(), cfg, db, user_id, bot_rx, rt);
    if let Err(e) = app.run() {
        eprintln!("TUI error: {}", e);
    }
}
