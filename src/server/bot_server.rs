//! Background bot server — polls Telegram, maintains DB-backed onboarding state
//! so questions never repeat across server restarts, and prevents duplicate entries.

use tokio::time::{sleep, Duration};
use serde::{Deserialize, Serialize};

use crate::config::Secrets;
use crate::memory::database::{Database, OnboardSession};

// ─── Public event type ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum BotEvent {
    TelegramPairing { chat_id: i64, username: String, text: String },
    DiscordPairing  { channel_id: String, username: String, text: String },
    Info(String),
}

// ─── Telegram API types ───────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct TgResponse<T> { ok: bool, result: Option<T> }

#[derive(Deserialize, Debug)]
struct TgUpdate { update_id: i64, message: Option<TgMessage> }

#[derive(Deserialize, Debug)]
struct TgMessage { chat: TgChat, from: Option<TgUser>, text: Option<String> }

#[derive(Deserialize, Debug, Clone)]
struct TgChat { id: i64 }

#[derive(Deserialize, Debug, Clone)]
struct TgUser { username: Option<String>, first_name: String }

// ─── Onboarding flow prompts ──────────────────────────────────────────────────

const STEPS: &[&str] = &[
    "👋 Hello! I'm the Omniscientia onboarding bot.\n\nWhat is your full name?",
    "📧 Thanks! What is your email address?",
    "🛠 What is your tech stack? (e.g. Rust, Python, React)",
    "🎯 What role are you requesting?\n\nOptions: Employee / Manager / Admin",
    "✅ All done! Your pairing request has been submitted.\nThe admin will review it shortly and notify you.",
];

// ─── Reply helper ─────────────────────────────────────────────────────────────

async fn tg_send(client: &reqwest::Client, base: &str, chat_id: i64, text: &str) {
    #[derive(Serialize)]
    struct Body<'a> { chat_id: i64, text: &'a str }
    let _ = client.post(format!("{}/sendMessage", base))
        .json(&Body { chat_id, text })
        .send().await;
}

// ─── BotServer ────────────────────────────────────────────────────────────────

pub struct BotServer {
    secrets: Secrets,
    db_path: String,
    tx:      tokio::sync::mpsc::Sender<BotEvent>,
}

impl BotServer {
    pub fn new(secrets: Secrets, db_path: String, tx: tokio::sync::mpsc::Sender<BotEvent>) -> Self {
        Self { secrets, db_path, tx }
    }

    pub fn start(self) {
        let token   = self.secrets.telegram_token.clone();
        let db_path = self.db_path.clone();
        let tx      = self.tx.clone();

        tokio::spawn(async move {
            if token.is_empty() { return; }
            let _ = tx.send(BotEvent::Info("Telegram bot polling started".into())).await;
            poll_telegram(token, db_path, tx).await;
        });
    }
}

// ─── Telegram long-poll loop ──────────────────────────────────────────────────

async fn poll_telegram(
    token:   String,
    db_path: String,
    tx:      tokio::sync::mpsc::Sender<BotEvent>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(40))
        .build().expect("reqwest client");

    let base = format!("https://api.telegram.org/bot{}", token);

    // Open own DB connection (SQLite supports multiple readers + WAL)
    let db = match Database::new(&db_path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BotEvent::Info(format!("[TG] DB open error: {}", e))).await;
            return;
        }
    };

    // Recover offset from existing sessions (don't re-process old messages)
    let mut offset: i64 = 0;

    loop {
        let url = format!("{}/getUpdates?offset={}&timeout=30", base, offset);
        let resp = match client.get(&url).send().await {
            Err(e) => {
                let _ = tx.send(BotEvent::Info(format!("[TG] poll error: {}", e))).await;
                sleep(Duration::from_secs(5)).await;
                continue;
            }
            Ok(r) => r,
        };

        let parsed: TgResponse<Vec<TgUpdate>> = match resp.json().await {
            Ok(p)  => p,
            Err(e) => {
                let _ = tx.send(BotEvent::Info(format!("[TG] parse error: {}", e))).await;
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        if !parsed.ok { sleep(Duration::from_secs(2)).await; continue; }

        for upd in parsed.result.unwrap_or_default() {
            offset = upd.update_id + 1;
            let Some(msg)  = upd.message else { continue };
            let Some(text) = msg.text     else { continue };
            let chat_id    = msg.chat.id;
            let chat_key   = chat_id.to_string();
            let tg_uname   = msg.from.as_ref()
                .and_then(|u| u.username.clone())
                .unwrap_or_else(|| msg.from.as_ref().map(|u| u.first_name.clone()).unwrap_or_default());

            // Load or create session from DB
            let mut session = db.load_onboard_session(&chat_key)
                .unwrap_or_default()
                .unwrap_or_else(|| OnboardSession {
                    chat_id: chat_key.clone(),
                    channel: "telegram".into(),
                    tg_username: tg_uname.clone(),
                    ..Default::default()
                });

            match session.step {
                0 => {
                    // Check if this user already submitted a complete request
                    let already_done = db.list_pending_users(None)
                        .unwrap_or_default()
                        .into_iter()
                        .any(|u| u.channel_user_id == chat_key && (u.status == "pending" || u.status == "approved"));

                    if already_done {
                        tg_send(&client, &base, chat_id,
                            "Your pairing request is already submitted! The admin will review it soon.").await;
                        continue;
                    }

                    session.tg_username = tg_uname.clone();
                    tg_send(&client, &base, chat_id, STEPS[0]).await;
                    session.step = 1;
                    let _ = db.save_onboard_session(&session);
                }
                1 => {
                    session.real_name = text.trim().to_string();
                    tg_send(&client, &base, chat_id, STEPS[1]).await;
                    session.step = 2;
                    let _ = db.save_onboard_session(&session);
                }
                2 => {
                    session.email = text.trim().to_string();
                    tg_send(&client, &base, chat_id, STEPS[2]).await;
                    session.step = 3;
                    let _ = db.save_onboard_session(&session);
                }
                3 => {
                    session.tech_stack = text.trim().to_string();
                    tg_send(&client, &base, chat_id, STEPS[3]).await;
                    session.step = 4;
                    let _ = db.save_onboard_session(&session);
                }
                4 => {
                    session.role_requested = text.trim().to_string();
                    tg_send(&client, &base, chat_id, STEPS[4]).await;

                    // Emit completed event → cli.rs persists to pending_users
                    let _ = tx.send(BotEvent::TelegramPairing {
                        chat_id,
                        username: session.tg_username.clone(),
                        text: format!("name={} | email={} | tech={} | role={}",
                            session.real_name, session.email,
                            session.tech_stack, session.role_requested),
                    }).await;

                    // Session step → 5 (done) — keep row so restarts remember "done"
                    session.step = 5;
                    let _ = db.save_onboard_session(&session);
                }
                _ => {
                    // Already done
                    tg_send(&client, &base, chat_id,
                        "Your pairing request is already submitted! The admin will review it soon.").await;
                }
            }
        }
    }
}
