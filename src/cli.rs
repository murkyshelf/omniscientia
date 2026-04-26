//! CLI subcommand handlers:
//!   omni server start|stop|status|install
//!   omni pairing list|approve|deny <username>
//!   omni help

use std::{fs, path::Path, process};

const PID_FILE:  &str = ".omniscientia/server.pid";
const DB_FILE:   &str = "omniscientia.db";

// ─── Help ─────────────────────────────────────────────────────────────────────

pub fn print_help() {
    println!(
r#"Omniscientia — Your organisation's AI agent

USAGE:
  omni                         Launch the TUI
  omni server start            Start the background bot server (foreground)
  omni server stop             Stop the running background server
  omni server status           Show whether the server is running
  omni server install          Print the systemd unit file
  omni pairing list            List all pending pairing requests
  omni pairing list --pending  Filter: pending only
  omni pairing list --approved Filter: approved only
  omni pairing approve <user>  Approve a pairing request
  omni pairing deny <user>     Deny a pairing request
  omni help                    Show this help

INSTALL AS SERVICE:
  omni server install | sudo tee /etc/systemd/system/omniscientia.service
  sudo systemctl daemon-reload
  sudo systemctl enable --now omniscientia
"#
    );
}

// ─── Server: start ────────────────────────────────────────────────────────────

pub fn server_start() {
    // Ensure .omniscientia dir exists
    let _ = fs::create_dir_all(".omniscientia");

    // Write PID file
    let pid = process::id();
    if let Err(e) = fs::write(PID_FILE, pid.to_string()) {
        eprintln!("[server] Failed to write PID file: {}", e);
        process::exit(1);
    }

    eprintln!("[omni-server] Starting (PID {})…", pid);

    // Load config + secrets
    crate::config::migrate_old_config();
    let secrets = crate::config::Secrets::load();

    let Some(cfg) = crate::config::AppConfig::load() else {
        eprintln!("[omni-server] No config.json found — run `omni` first to set up.");
        let _ = fs::remove_file(PID_FILE);
        process::exit(1);
    };

    let db = match crate::memory::database::Database::new(DB_FILE) {
        Ok(d) => d,
        Err(e) => { eprintln!("[omni-server] DB error: {}", e); process::exit(1); }
    };
    // Ensure Admin user exists
    let _ = db.upsert_user("Admin", 4);

    eprintln!("[omni-server] Connected to DB. Starting bot loops…");
    eprintln!("[omni-server] Ollama: {}  Model: {}", cfg.ollama_base, cfg.model_name);

    // Build runtime and run bot server forever
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::server::BotEvent>(128);
        crate::server::BotServer::new(secrets, DB_FILE.to_string(), tx).start();

        eprintln!("[omni-server] Bot server running. Ctrl-C to stop.");

        // Persist incoming bot events to DB
        while let Some(evt) = rx.recv().await {
            match evt {
                crate::server::BotEvent::TelegramPairing { chat_id, username, text } => {
                    eprintln!("[TG] @{} ({}) completed onboarding: {}", username, chat_id, text);
                    // Parse "name=X | email=Y | tech=Z | role=W"
                    let mut name = String::new();
                    let mut email = String::new();
                    let mut tech = String::new();
                    let mut role = "Employee".to_string();
                    for part in text.split(" | ") {
                        let mut kv = part.splitn(2, '=');
                        match (kv.next(), kv.next()) {
                            (Some("name"), Some(v))  => name  = v.to_string(),
                            (Some("email"), Some(v)) => email = v.to_string(),
                            (Some("tech"), Some(v))  => tech  = v.to_string(),
                            (Some("role"), Some(v))  => role  = v.to_string(),
                            _ => {}
                        }
                    }
                    let _ = db.add_pending_user_full(
                        &username, &name, &email, &tech, &role,
                        "telegram", &chat_id.to_string(),
                    );
                }
                crate::server::BotEvent::DiscordPairing { channel_id, username, text } => {
                    eprintln!("[DC] @{} ({}): {}", username, channel_id, text);
                    let _ = db.add_pending_user(&username, "discord", &channel_id);
                }
                crate::server::BotEvent::Info(msg) => {
                    eprintln!("[omni-server] {}", msg);
                }
            }
        }
    });

    let _ = fs::remove_file(PID_FILE);
}

// ─── Server: stop ─────────────────────────────────────────────────────────────

pub fn server_stop() {
    match fs::read_to_string(PID_FILE) {
        Err(_) => println!("Server is not running (no PID file found)."),
        Ok(pid_str) => {
            let pid: u32 = match pid_str.trim().parse() {
                Ok(p) => p,
                Err(_) => { eprintln!("Invalid PID file."); return; }
            };
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            #[cfg(windows)]
            { eprintln!("stop not supported on Windows"); }
            let _ = fs::remove_file(PID_FILE);
            println!("Sent SIGTERM to PID {}.", pid);
        }
    }
}

// ─── Server: status ───────────────────────────────────────────────────────────

pub fn server_status() {
    match fs::read_to_string(PID_FILE) {
        Err(_) => println!("omni-server  STOPPED  (no PID file)"),
        Ok(pid_str) => {
            let pid: u32 = pid_str.trim().parse().unwrap_or(0);
            // Check /proc/<pid> on Linux
            let alive = Path::new(&format!("/proc/{}", pid)).exists();
            if alive {
                println!("omni-server  RUNNING  PID {}", pid);
            } else {
                println!("omni-server  DEAD  (stale PID {})", pid);
                let _ = fs::remove_file(PID_FILE);
            }
        }
    }
}

// ─── Server: install (systemd unit) ──────────────────────────────────────────

pub fn systemd_print() {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/omni".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/opt/omniscientia".to_string());

    println!(
r#"[Unit]
Description=Omniscientia Background Bot Server
After=network.target

[Service]
Type=simple
ExecStart={exe} server start
WorkingDirectory={cwd}
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"#
    );
}

// ─── Pairing: list ────────────────────────────────────────────────────────────

pub fn pairing_list(filter: Option<&str>) {
    let db = match crate::memory::database::Database::new(DB_FILE) {
        Ok(d) => d,
        Err(e) => { eprintln!("DB error: {}", e); return; }
    };

    let status_filter = match filter {
        Some("--pending")  => Some("pending"),
        Some("--approved") => Some("approved"),
        Some("--denied")   => Some("denied"),
        _ => None,
    };

    let users = match db.list_pending_users(status_filter) {
        Ok(u) => u,
        Err(e) => { eprintln!("Query error: {}", e); return; }
    };

    if users.is_empty() {
        println!("No pairing requests found.");
        return;
    }

    println!("{:<4} {:<20} {:<10} {:<10} {:<20}",
             "ID", "Username", "Channel", "Status", "Created");
    println!("{}", "-".repeat(70));
    for u in &users {
        println!("{:<4} {:<20} {:<10} {:<10} {:<20}",
                 u.id, u.username, u.channel, u.status,
                 &u.created_at[..u.created_at.len().min(19)]);
    }
    println!("\n{} request(s) total.", users.len());
}

// ─── Pairing: approve / deny ──────────────────────────────────────────────────

pub fn pairing_approve(username: &str) {
    update_pairing(username, "approved");
    println!("Approved pairing request for @{}.", username);
}

pub fn pairing_deny(username: &str) {
    update_pairing(username, "denied");
    println!("Denied pairing request for @{}.", username);
}

fn update_pairing(username: &str, status: &str) {
    let db = match crate::memory::database::Database::new(DB_FILE) {
        Ok(d) => d,
        Err(e) => { eprintln!("DB error: {}", e); return; }
    };
    // Find by username → get id
    match db.list_pending_users(None) {
        Ok(users) => {
            let matched: Vec<_> = users.iter().filter(|u| u.username == username).collect();
            if matched.is_empty() { eprintln!("No request found for @{}.", username); return; }
            for u in matched {
                if let Err(e) = db.update_pending_user_status(u.id, status) {
                    eprintln!("Update error: {}", e);
                }
            }
        }
        Err(e) => eprintln!("Query error: {}", e),
    }
}
