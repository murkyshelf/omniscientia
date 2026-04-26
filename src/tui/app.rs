#![allow(dead_code, unused_imports)]

use crate::server::BotEvent;
use std::sync::Arc;
use tokio::runtime::Runtime;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{error::Error, io};

use crate::config::AppConfig;
use crate::llm::provider::LlmProvider;
use crate::llm::prompt::build_system_prompt;
use crate::memory::database::Database;
use crate::tools::executor::Executor;

// ─── Screens ──────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Screen {
    Home,
    Chat,
    Config,
    Pairing,
    DevMenu,
    DbViewer,
    UserRecords,
}

// ─── App struct ───────────────────────────────────────────────────────────────

pub struct App {
    pub running: bool,
    pub screen:  Screen,

    // identity
    pub alias:   String,
    pub config:  AppConfig,
    pub db:      Database,
    pub uid:     i64,

    // Home / DevMenu selection
    pub home_sel: usize,
    pub dev_sel:  usize,

    // Chat state — messages store raw text; AI responses tagged with "omni:" prefix
    pub messages:    Vec<String>,
    pub input:       String,
    pub cursor:      usize,
    pub waiting:     bool,
    pub scroll:      usize,

    // Autocomplete popup
    pub ac_items: Option<Vec<String>>,
    pub ac_sel:   usize,

    // Bot event channel from background server
    pub bot_rx: tokio::sync::mpsc::Receiver<BotEvent>,
    // Shared tokio runtime (passed from main, no nested runtime)
    pub rt: Arc<Runtime>,

    // Pairing screen state
    pub pairing_list: Vec<crate::memory::database::PendingUser>,
    pub pairing_sel:  usize,

    // Dev menu: live server status
    pub server_pid: Option<u32>,
}

const SLASH_CMDS: &[&str] = &["/quit", "/clear", "/help", "/config", "/memory", "/tools"];

impl App {
    pub fn new(alias: String, config: AppConfig, db: Database, uid: i64,
               bot_rx: tokio::sync::mpsc::Receiver<BotEvent>,
               rt: Arc<Runtime>) -> Self {
        let mut messages = vec![
            format!("Welcome to Omniscientia, {}!", alias),
            format!("Model: {}  |  {}", config.model_name, config.ollama_base),
            String::from("Type /help for commands  ·  Esc returns to the main menu"),
            String::from("-".repeat(50)),
        ];
        if let Ok(hist) = db.load_recent_messages(uid, 50) {
            for m in &hist {
                let who = if m.role == "user" { alias.as_str() } else { "omni" };
                messages.push(format!("{}: {}", who, m.content));
            }
        }
        Self {
            running: true, screen: Screen::Home,
            alias, config, db, uid,
            home_sel: 0, dev_sel: 0,
            messages, input: String::new(), cursor: 0, waiting: false, scroll: 0,
            ac_items: None, ac_sel: 0,
            bot_rx, rt,
            pairing_list: Vec::new(), pairing_sel: 0,
            server_pid: read_server_pid(),
        }
    }

    // ─── Main event loop ──────────────────────────────────────────────────────

    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        let mut term = Terminal::new(CrosstermBackend::new(out))?;
        // Use the shared runtime from main — no nested Runtime::new() here
        let rt = self.rt.clone();

        while self.running {
            // Drain any bot events from the background server (non-blocking)
            while let Ok(evt) = self.bot_rx.try_recv() {
                match evt {
                    BotEvent::TelegramPairing { username, text, .. } => {
                        self.messages.push(format!("[TG] @{} wants to pair: {}", username, text));
                    }
                    BotEvent::DiscordPairing { username, text, .. } => {
                        self.messages.push(format!("[DC] @{} wants to pair: {}", username, text));
                    }
                    BotEvent::Info(msg) => {
                        self.messages.push(format!("[Bot] {}", msg));
                    }
                }
            }

            term.draw(|f| self.draw(f))?;
            if let Event::Key(k) = event::read()? {
                match self.screen {
                    Screen::Home     => self.on_home(k.code),
                    Screen::DevMenu  => self.on_dev(k.code),
                    Screen::Chat     => self.on_chat(k.code, &rt, &mut term)?,
                    Screen::Config   => {
                        // Tear down TUI, run wizard, rebuild TUI
                        disable_raw_mode()?;
                        execute!(term.backend_mut(), LeaveAlternateScreen)?;
                        let _ = run_config_wizard();
                        if let Some(cfg) = crate::config::AppConfig::load() { self.config = cfg; }
                        enable_raw_mode()?;
                        execute!(io::stdout(), EnterAlternateScreen)?;
                        term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                        self.screen = Screen::Home;
                    }
                    Screen::Pairing  => self.on_pairing(k.code),
                    Screen::DbViewer | Screen::UserRecords
                                     => if matches!(k.code, KeyCode::Esc | KeyCode::Char('q')) { self.screen = Screen::DevMenu; }
                }
            }
        }

        disable_raw_mode()?;
        execute!(term.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    // ─── Home ─────────────────────────────────────────────────────────────────

    fn on_home(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up   => self.home_sel = self.home_sel.saturating_sub(1),
            KeyCode::Down => self.home_sel = (self.home_sel + 1).min(3),
            KeyCode::Enter => {
                self.screen = match self.home_sel {
                    0 => Screen::Chat,
                    1 => Screen::Config,   // launches the config wizard in run()
                    2 => Screen::Pairing,
                    3 => Screen::DevMenu,
                    _ => Screen::Home,
                };
            }
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            _ => {}
        }
    }

    fn on_dev(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up   => self.dev_sel = self.dev_sel.saturating_sub(1),
            KeyCode::Down => self.dev_sel = (self.dev_sel + 1).min(3),
            KeyCode::Enter => {
                match self.dev_sel {
                    0 => self.screen = Screen::DbViewer,
                    1 => self.screen = Screen::UserRecords,
                    2 => self.toggle_server(),
                    _ => self.screen = Screen::Home,
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Home,
            _ => {}
        }
    }

    fn toggle_server(&mut self) {
        self.server_pid = read_server_pid();
        if self.server_pid.is_some() {
            // Stop
            let _ = std::process::Command::new(
                std::env::current_exe().unwrap_or_else(|_| "omni".into())
            ).arg("server").arg("stop").status();
            self.server_pid = None;
        } else {
            // Start detached
            if let Ok(child) = std::process::Command::new(
                std::env::current_exe().unwrap_or_else(|_| "omni".into())
            ).arg("server").arg("start")
             .stdin(std::process::Stdio::null())
             .stdout(std::process::Stdio::null())
             .stderr(std::process::Stdio::null())
             .spawn() {
                // Give it a moment then re-read PID
                std::thread::sleep(std::time::Duration::from_millis(400));
                self.server_pid = read_server_pid().or(Some(child.id()));
            }
        }
    }


    // ─── Chat input ───────────────────────────────────────────────────────────

    fn on_chat(
        &mut self, key: KeyCode,
        rt: &tokio::runtime::Runtime,
        term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<(), Box<dyn Error>> {

        // Autocomplete active?
        if self.ac_items.is_some() {
            match key {
                KeyCode::Up => {
                    self.ac_sel = self.ac_sel.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Down => {
                    let n = self.ac_items.as_ref().map_or(1, |v| v.len());
                    self.ac_sel = (self.ac_sel + 1).min(n.saturating_sub(1));
                    return Ok(());
                }
                KeyCode::Tab | KeyCode::Enter => {
                    if let Some(ref items) = self.ac_items.clone() {
                        if let Some(pick) = items.get(self.ac_sel) {
                            let trigger = self.input[..self.cursor]
                                .rfind(|c: char| c == '/' || c == '@')
                                .unwrap_or(self.cursor);
                            self.input.truncate(trigger);
                            self.input.push_str(pick);
                            self.cursor = self.input.len();
                        }
                    }
                    self.ac_items = None;
                    return Ok(());
                }
                KeyCode::Esc => { self.ac_items = None; return Ok(()); }
                _ => {}
            }
        }

        // Scroll
        if !self.waiting {
            match key {
                KeyCode::PageUp | KeyCode::Up   => { self.scroll = self.scroll.saturating_add(1); return Ok(()); }
                KeyCode::PageDown | KeyCode::Down if self.scroll > 0 => { self.scroll -= 1; return Ok(()); }
                _ => {}
            }
        }

        if self.waiting { return Ok(()); }

        match key {
            KeyCode::Esc  => { self.screen = Screen::Home; }
            KeyCode::Left  => { self.cursor = self.cursor.saturating_sub(1); }
            KeyCode::Right => { if self.cursor < self.input.len() { self.cursor += 1; } }
            KeyCode::Home  => { self.cursor = 0; }
            KeyCode::End   => { self.cursor = self.input.len(); }
            KeyCode::Delete => { if self.cursor < self.input.len() { self.input.remove(self.cursor); } }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                    self.update_ac();
                }
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
                self.update_ac();
            }
            KeyCode::Enter => {
                let msg: String = self.input.drain(..).collect();
                self.cursor = 0;
                self.ac_items = None;
                self.scroll = 0;
                self.dispatch(&msg, rt, term)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn update_ac(&mut self) {
        let view = self.input[..self.cursor].to_string();
        if let Some(p) = view.rfind('/') {
            let typed = &view[p..];
            let v: Vec<String> = SLASH_CMDS.iter().filter(|c| c.starts_with(typed)).map(|s| s.to_string()).collect();
            if !v.is_empty() { self.ac_items = Some(v); self.ac_sel = 0; return; }
        }
        if let Some(p) = view.rfind('@') {
            let typed = &view[p + 1..];
            let v: Vec<String> = self.db.list_usernames().unwrap_or_default()
                .into_iter().filter(|u| u.starts_with(typed))
                .map(|u| format!("@{}", u)).collect();
            if !v.is_empty() { self.ac_items = Some(v); self.ac_sel = 0; return; }
        }
        self.ac_items = None;
    }

    fn dispatch(
        &mut self, msg: &str,
        rt: &tokio::runtime::Runtime,
        term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<(), Box<dyn Error>> {
        match msg.trim() {
            "/quit"  => { self.running = false; return Ok(()); }
            "/clear" => {
                let pfx = format!("{}:", self.alias);
                self.messages.retain(|m| !m.starts_with(&pfx) && !m.starts_with("Omniscientia:"));
                return Ok(());
            }
            "/help" => {
                self.messages.push("── Commands ──".into());
                for c in SLASH_CMDS { self.messages.push(format!("  {}", c)); }
                return Ok(());
            }
            "" => return Ok(()),
            _ => {}
        }

        let _ = self.db.save_message(self.uid, "user", msg);
        self.messages.push(format!("{}: {}", self.alias, msg));
        self.waiting = true;
        term.draw(|f| self.draw(f))?;

        let sys = build_system_prompt(&self.alias, "Admin", None);
        let ctx: Vec<(String, String)> = self.db.load_recent_messages(self.uid, 20)
            .unwrap_or_default().into_iter().map(|m| (m.role, m.content)).collect();
        let provider = LlmProvider::new(&self.config.ollama_base, &self.config.model_name);

        match rt.block_on(provider.chat_with_context(&sys, &ctx, msg)) {
            Ok(resp) => {
                if let Some(call) = Executor::parse_tool_call(&resp) {
                    let res = Executor::new(3).execute(&call);
                    self.messages.push(format!("[Tool] {} → {}", call.tool, &res[..res.len().min(120)]));
                    let mut ctx2 = ctx;
                    ctx2.extend([("user".into(), msg.to_string()), ("assistant".into(), resp), ("user".into(), format!("[tool_result]\n{}", res))]);
                    if let Ok(r) = rt.block_on(provider.chat_with_context(&sys, &ctx2, "Summarise the tool result for the user.")) {
                        let _ = self.db.save_message(self.uid, "assistant", &r);
                        self.messages.push(format!("omni: {}", r));
                    }
                } else {
                    let _ = self.db.save_message(self.uid, "assistant", &resp);
                    self.messages.push(format!("omni: {}", resp));
                }
            }
            Err(e) => self.messages.push(format!("[ERROR] {}", e)),
        }
        self.waiting = false;
        Ok(())
    }

    // ─── Draw ─────────────────────────────────────────────────────────────────

    fn draw(&self, f: &mut Frame) {
        match self.screen {
            Screen::Home        => self.draw_home(f),
            Screen::DevMenu     => self.draw_dev_menu(f),
            Screen::Chat        => self.draw_chat(f),
            Screen::Config      => {} // wizard runs synchronously in run(); nothing to render
            Screen::Pairing     => self.draw_pairing(f),
            Screen::DbViewer    => self.draw_stub(f, "[D] DB Viewer",  "Browse the SQLite database visually.\n\nComing in Phase 2.\n\nPress q or Esc to go back."),
            Screen::UserRecords => self.draw_stub(f, "[U] User Records","All registered users and their details.\n\nComing in Phase 2.\n\nPress q or Esc to go back."),
        }
    }

    // ── Home screen ───────────────────────────────────────────────────────────

    fn draw_home(&self, f: &mut Frame) {
        let area = f.area();
        // No forced background — use terminal default

        // Layout rows
        let rows = Layout::default()
            .constraints([
                Constraint::Length(6),   // logo
                Constraint::Length(1),   // tagline
                Constraint::Length(1),   // spacer
                Constraint::Length(8),   // menu
                Constraint::Min(0),      // remaining
                Constraint::Length(1),   // hint
            ])
            .split(area);

        // Responsive centered logo
        self.render_logo(f, rows[0], area.width);

        // Tagline
        f.render_widget(
            Paragraph::new("Your organisation's AI -- always watching, always ready.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            rows[1],
        );

        // Plain-ASCII menu items (safe for any terminal)
        let items: &[(&str, &str)] = &[
            ("[>] Chat",    "Start or continue the admin chat"),
            ("[*] Config",  "Edit model, URL and API tokens"),
            ("[@] Pairing", "Onboard users from Telegram / Discord"),
            ("[/] Dev",     "Database viewer and user records"),
        ];
        let menu_w = 62u16.min(area.width);
        let menu_x = area.width.saturating_sub(menu_w) / 2;
        let menu_rect = Rect { x: menu_x, y: rows[3].y, width: menu_w, height: rows[3].height };

        let lines: Vec<Line> = items.iter().enumerate().map(|(i, (label, desc))| {
            let active = i == self.home_sel;
            if active {
                Line::from(vec![
                    Span::styled(format!(" >> {:<20}", label), Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {}", desc), Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!("    {:<20}", label), Style::default().fg(Color::White)),
                    Span::styled(format!("  {}", desc), Style::default().fg(Color::DarkGray)),
                ])
            }
        }).collect();

        f.render_widget(
            Paragraph::new(lines)
                .block(Block::default()
                    .title(" Menu ")
                    .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))),
            menu_rect,
        );

        // Hint
        f.render_widget(
            Paragraph::new("[Up]/[Dn] move   Enter select   q quit")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            rows[5],
        );
    }

    // ── Dev submenu ───────────────────────────────────────────────────────────

    fn draw_dev_menu(&self, f: &mut Frame) {
        let area = f.area();
        let rows = Layout::default()
            .constraints([Constraint::Length(6), Constraint::Length(1), Constraint::Length(1), Constraint::Length(9), Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        self.render_logo(f, rows[0], area.width);
        let (srv_label, srv_style) = if self.server_pid.is_some() {
            (" [SERVER: RUNNING] ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            (" [SERVER: STOPPED] ", Style::default().fg(Color::Black).bg(Color::DarkGray))
        };
        f.render_widget(Paragraph::new(srv_label).alignment(Alignment::Center).style(srv_style), rows[1]);
        let srv_action = if self.server_pid.is_some() { "[S] Stop Server" } else { "[S] Start Server" };
        let items: Vec<(&str, &str)> = vec![
            ("[D] Database Viewer", "Browse all SQLite tables visually"),
            ("[U] User Records",    "View users registered via channels"),
            (srv_action,           "Toggle the background bot server"),
            ("[<] Back",           "Return to the main menu"),
        ];
        let menu_w = 62u16.min(area.width);
        let menu_x = area.width.saturating_sub(menu_w) / 2;
        let menu_rect = Rect { x: menu_x, y: rows[3].y, width: menu_w, height: rows[3].height };
        let lines: Vec<Line> = items.iter().enumerate().map(|(i, (label, desc))| {
            let active = i == self.dev_sel;
            let hl = if i == 2 {
                if self.server_pid.is_some() { Color::Green } else { Color::Yellow }
            } else { Color::Yellow };
            if active {
                Line::from(vec![
                    Span::styled(format!(" >> {:<24}", label), Style::default().fg(Color::Black).bg(hl).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {}", desc), Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!("    {:<24}", label), Style::default().fg(Color::White)),
                    Span::styled(format!("  {}", desc), Style::default().fg(Color::DarkGray)),
                ])
            }
        }).collect();
        f.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title(" Dev Tools ").title_style(Style::default().fg(Color::Yellow))
                    .borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
            menu_rect,
        );
        f.render_widget(
            Paragraph::new("[Up]/[Dn] move   Enter select   Esc back")
                .alignment(Alignment::Center).style(Style::default().fg(Color::DarkGray)),
            rows[5],
        );
    }

    // ── Pairing / Approvals screen ────────────────────────────────────────────

    fn draw_pairing(&self, f: &mut Frame) {
        let area = f.area();
        let chunks = Layout::default()
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        let hdr = Line::from(Span::styled(
            format!("  {:<4} {:<20} {:<10} {:<10}", "ID", "Username", "Channel", "Status"),
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        let mut body: Vec<Line> = vec![hdr];
        for (i, u) in self.pairing_list.iter().enumerate() {
            let sc = match u.status.as_str() {
                "approved" => Style::default().fg(Color::Green),
                "denied"   => Style::default().fg(Color::Red),
                _          => Style::default().fg(Color::Yellow),
            };
            let text = format!("  {:<4} {:<20} {:<10} {:<10}", u.id, u.username, u.channel, u.status);
            body.push(if i == self.pairing_sel {
                Line::from(Span::styled(text, Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(Span::styled(text, sc))
            });
        }
        if self.pairing_list.is_empty() {
            body.push(Line::from(Span::styled("  No pairing requests yet.", Style::default().fg(Color::DarkGray))));
        }
        let pending = self.pairing_list.iter().filter(|u| u.status == "pending").count();
        f.render_widget(
            Paragraph::new(body).wrap(Wrap { trim: false })
                .block(Block::default()
                    .title(format!(" Pairing Requests  ({} pending) ", pending))
                    .title_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
                    .borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
            chunks[0],
        );
        f.render_widget(
            Paragraph::new("[a] Approve   [d] Deny   [r] Refresh   Esc back")
                .alignment(Alignment::Center).style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }

    // ── Chat screen ───────────────────────────────────────────────────────────

    fn draw_chat(&self, f: &mut Frame) {
        let area = f.area();

        // Status | body | input
        let outer = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(3)])
            .split(area);

        // Status bar
        let now = chrono::Local::now().format("%H:%M");
        f.render_widget(
            Paragraph::new(format!(" ◈ Omniscientia  │  {}  │  {}  │  {} ", self.alias, self.config.model_name, now))
                .style(Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD)),
            outer[0],
        );

        // Body: chat pane (68%) + sidebar (32%)
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(outer[1]);

        // Chat messages
        let vis_h = body[0].height.saturating_sub(2) as usize;
        let total  = self.messages.len();
        let start  = total.saturating_sub(vis_h + self.scroll);
        let end    = total.saturating_sub(self.scroll);

        let mut msg_lines: Vec<Line> = Vec::new();
        let pfx = format!("{}:", self.alias);
        for m in &self.messages[start..end] {
            if m.starts_with(&pfx) {
                // User message
                msg_lines.push(Line::from(vec![
                    Span::styled(" > ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::styled(m.clone(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                ]));
            } else if let Some(content) = m.strip_prefix("omni:") {
                // AI response — render as markdown
                msg_lines.push(Line::from(Span::styled(
                    " Omniscientia ".to_string(),
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
                )));
                for line in crate::tui::markdown::render_markdown(content.trim_start()) {
                    msg_lines.push(line);
                }
                msg_lines.push(Line::from(""));
            } else if m.starts_with("[TG]") || m.starts_with("[DC]") {
                msg_lines.push(Line::from(Span::styled(m.clone(), Style::default().fg(Color::Magenta))));
            } else if m.starts_with("[Bot]") {
                msg_lines.push(Line::from(Span::styled(m.clone(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))));
            } else if m.starts_with("[Tool]") {
                msg_lines.push(Line::from(Span::styled(m.clone(), Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))));
            } else if m.starts_with("[ERROR]") {
                msg_lines.push(Line::from(vec![
                    Span::styled("⚠ ", Style::default().fg(Color::Red)),
                    Span::styled(m.clone(), Style::default().fg(Color::Red)),
                ]));
            } else if m.starts_with("--") || m.starts_with("─") {
                msg_lines.push(Line::from(Span::styled(m.clone(), Style::default().fg(Color::DarkGray))));
            } else {
                msg_lines.push(Line::from(Span::styled(m.clone(), Style::default().fg(Color::Gray))));
            }
        }


        let chat_title = if self.scroll > 0 { format!(" ↑ scrolled {}  |  ↓/PgDn to return ", self.scroll) }
            else if self.waiting { " ⟳ Thinking… ".to_string() }
            else { " Chat  —  Esc for menu ".to_string() };

        f.render_widget(
            Paragraph::new(msg_lines).wrap(Wrap { trim: false })
                .block(Block::default().title(chat_title)
                    .title_style(if self.waiting { Style::default().fg(Color::Yellow) } else { Style::default().fg(Color::Cyan) })
                    .borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
            body[0],
        );

        // Sidebar
        f.render_widget(
            Paragraph::new(self.sidebar()).wrap(Wrap { trim: false })
                .block(Block::default().title(" Context ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
            body[1],
        );

        // Input bar with cursor
        let mut spans: Vec<Span> = vec![];
        for (i, ch) in self.input.chars().enumerate() {
            if i == self.cursor { spans.push(Span::styled(ch.to_string(), Style::default().bg(Color::Yellow).fg(Color::Black))); }
            else                { spans.push(Span::raw(ch.to_string())); }
        }
        if self.cursor == self.input.len() && !self.waiting {
            spans.push(Span::styled("▌", Style::default().fg(Color::Yellow)));
        }
        let in_title = if self.waiting { " ⟳ Please wait… ".to_string() }
            else { format!(" › message  /command  @mention  [{} chars] ", self.input.len()) };
        f.render_widget(
            Paragraph::new(Line::from(spans))
                .block(Block::default().title(in_title).borders(Borders::ALL)
                    .border_style(Style::default().fg(if self.waiting { Color::DarkGray } else { Color::Yellow }))),
            outer[2],
        );

        // Autocomplete popup
        if let Some(ref ac) = self.ac_items {
            let ph = (ac.len() as u16 + 2).min(10);
            let pw = ac.iter().map(|s| s.len()).max().unwrap_or(10) as u16 + 4;
            let pw = pw.min(area.width.saturating_sub(4));
            let py = area.height.saturating_sub(3 + ph);
            let popup = Rect { x: 4, y: py, width: pw, height: ph };
            f.render_widget(Clear, popup);
            let items: Vec<ListItem> = ac.iter().enumerate().map(|(i, s)| {
                ListItem::new(Span::styled(s.clone(),
                    if i == self.ac_sel { Style::default().fg(Color::Black).bg(Color::Cyan) }
                    else { Style::default().fg(Color::White) }))
            }).collect();
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))),
                popup,
            );
        }
    }

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn render_logo(&self, f: &mut Frame, area: Rect, term_w: u16) {
        // Pick the logo that fits the terminal width
        let logo_text = if term_w >= 68 { LOGO } else { LOGO_SHORT };
        // All chars are single-byte ASCII so len() == chars().count()
        let logo_w = logo_text.lines().map(|l| l.len()).max().unwrap_or(0) as u16;
        let logo_x = term_w.saturating_sub(logo_w) / 2;
        let logo_rect = Rect {
            x: logo_x,
            y: area.y,
            width: logo_w.min(term_w.saturating_sub(logo_x)),
            height: area.height.min(logo_text.lines().count() as u16 + 1),
        };
        f.render_widget(
            Paragraph::new(logo_text).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            logo_rect,
        );
    }

    fn draw_stub(&self, f: &mut Frame, title: &str, body: &str) {
        let area = f.area();
        let outer = Layout::default()
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        f.render_widget(
            Paragraph::new(format!("\n\n{}", body)).alignment(Alignment::Center).wrap(Wrap { trim: false })
                .block(Block::default().title(format!(" {} ", title)).borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
                .style(Style::default().fg(Color::Gray)),
            outer[0],
        );
        f.render_widget(
            Paragraph::new("q / Esc  ←  back").alignment(Alignment::Center).style(Style::default().fg(Color::DarkGray)),
            outer[1],
        );
    }

    fn sidebar(&self) -> Vec<Line<'static>> {
        let mut v: Vec<Line> = vec![];
        v.push(Line::from(Span::styled("Session", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        v.push(Line::from(vec![Span::styled("  User  ", Style::default().fg(Color::DarkGray)), Span::styled(self.alias.clone(), Style::default().fg(Color::White))]));
        v.push(Line::from(vec![Span::styled("  Role  ", Style::default().fg(Color::DarkGray)), Span::styled("Admin", Style::default().fg(Color::Green))]));
        v.push(Line::from(vec![Span::styled("  Model ", Style::default().fg(Color::DarkGray)), Span::styled(self.config.model_name.clone(), Style::default().fg(Color::Yellow))]));
        v.push(Line::from(""));
        v.push(Line::from(Span::styled("Memory", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        for f in &["memory/SOLE.md", "memory/skills.md", "memory/tools.md"] {
            let ok = std::path::Path::new(f).exists();
            v.push(Line::from(vec![
                Span::styled(if ok { "  ✓ " } else { "  ✗ " }, Style::default().fg(if ok { Color::Green } else { Color::Red })),
                Span::styled(f.replace("memory/", ""), Style::default().fg(Color::Gray)),
            ]));
        }
        v.push(Line::from(""));
        v.push(Line::from(Span::styled("Tools", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        for t in &["read_file", "write_file", "shell_exec", "python_exec", "notify"] {
            v.push(Line::from(vec![Span::styled("  ⚙ ", Style::default().fg(Color::Yellow)), Span::styled(*t, Style::default().fg(Color::Gray))]));
        }
        v.push(Line::from(""));
        let mc = self.messages.iter().filter(|m| m.contains(':') && !m.starts_with('─')).count();
        v.push(Line::from(Span::styled("Stats", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        v.push(Line::from(vec![Span::styled("  Msgs  ", Style::default().fg(Color::DarkGray)), Span::styled(mc.to_string(), Style::default().fg(Color::White))]));
        v
    }
}

// ─── Pairing event handler ────────────────────────────────────────────────────

impl App {
    fn on_pairing(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up   => self.pairing_sel = self.pairing_sel.saturating_sub(1),
            KeyCode::Down => {
                if !self.pairing_list.is_empty() {
                    self.pairing_sel = (self.pairing_sel + 1).min(self.pairing_list.len() - 1);
                }
            }
            KeyCode::Char('r') => {
                // Refresh pairing list from DB
                self.pairing_list = self.db.list_pending_users(None).unwrap_or_default();
                self.pairing_sel = 0;
            }
            KeyCode::Char('a') => {
                if let Some(u) = self.pairing_list.get(self.pairing_sel).cloned() {
                    let _ = self.db.update_pending_user_status(u.id, "approved");
                    // Also create them as a real user
                    let _ = self.db.upsert_user(&u.username, 2); // Employee role
                    self.pairing_list = self.db.list_pending_users(None).unwrap_or_default();
                }
            }
            KeyCode::Char('d') => {
                if let Some(u) = self.pairing_list.get(self.pairing_sel).cloned() {
                    let _ = self.db.update_pending_user_status(u.id, "denied");
                    self.pairing_list = self.db.list_pending_users(None).unwrap_or_default();
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Home;
            }
            _ => {}
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn read_server_pid() -> Option<u32> {
    let pid_str = std::fs::read_to_string(".omniscientia/server.pid").ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    // Verify the process is actually alive
    if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(".omniscientia/server.pid");
        None
    }
}

// ─── Logo — clean bordered banner (no backslashes, renders in any terminal) ──

// Full logo shown when terminal is 68+ columns wide (exactly 64 chars wide)
const LOGO: &str = "\
+--------------------------------------------------------------+\n\
|  >>>  O M N I S C I E N T I A                          <<<  |\n\
|       Your organisation's AI agent                          |\n\
+--------------------------------------------------------------+";

// Short logo shown when terminal is under 68 columns wide (exactly 24 chars wide)
const LOGO_SHORT: &str = "\
+----------------------+\n\
|  >>>  O M N I  <<<  |\n\
|       Omniscientia   |\n\
+----------------------+";

// ─── Ollama model discovery ────────────────────────────────────────────────────

fn fetch_ollama_models(base_url: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/').trim_end_matches("/api/chat").trim_end_matches("/api/generate");
    // Use block_in_place so this works whether called from sync or async context
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().ok()?;
            let resp = client.get(format!("{}/api/tags", base)).send().await.ok()?;
            let json: serde_json::Value = resp.json().await.ok()?;
            let names: Vec<String> = json["models"].as_array()?.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect();
            if names.is_empty() { None } else { Some(names) }
        })
    }).unwrap_or_default()
}

// ─── Config wizard ────────────────────────────────────────────────────────────

enum WizStep { Url, Model, Tg, Discord }

pub fn run_config_wizard() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let mut step = WizStep::Url;
    let mut url  = String::new();
    let mut tg   = String::new();
    let mut disc = String::new();
    let mut models: Vec<String> = vec![];
    let mut mstate = ListState::default();
    let mut mstatus = "Enter Ollama URL then press Enter".to_string();
    let mut done = false;

    while !done {
        let ur = url.clone(); let tgr = tg.clone(); let dr = disc.clone();
        let msr = mstatus.clone(); let mlr = models.clone();
        let snum: usize = match &step { WizStep::Url => 1, WizStep::Model => 2, WizStep::Tg => 3, WizStep::Discord => 4 };

        term.draw(|f| {
            let a = f.area();
            // No forced background

            let rows = Layout::default()
                .constraints([Constraint::Length(6), Constraint::Length(2), Constraint::Min(5)])
                .split(a);

            // Responsive centred logo
            let logo_text = if a.width >= 68 { LOGO } else { LOGO_SHORT };
            let lw = logo_text.lines().map(|l| l.len()).max().unwrap_or(0) as u16;
            let lx = a.width.saturating_sub(lw) / 2;
            f.render_widget(
                Paragraph::new(logo_text).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Rect { x: lx, y: rows[0].y, width: lw.min(a.width.saturating_sub(lx)), height: rows[0].height },
            );

            let hint = format!(
                " Step {}/4 — {}  |  ↑↓ models  |  Enter  |  Esc skip",
                snum,
                match snum { 1 => "Ollama base URL", 2 => "Pick model", 3 => "Telegram token (saved to .env)", _ => "Discord token (saved to .env)" }
            );
            f.render_widget(Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)), rows[1]);

            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
                .split(rows[2]);

            let sel_model = if snum >= 2 && !mlr.is_empty() {
                mstate.selected().and_then(|i| mlr.get(i)).cloned().unwrap_or_default()
            } else { String::new() };

            let active_val = match snum { 1 => ur.as_str(), 3 => tgr.as_str(), 4 => dr.as_str(), _ => "" };
            f.render_widget(
                Paragraph::new(vec![
                    wiz_field("1. Ollama URL",     &ur,        snum == 1),
                    wiz_field("2. AI Model",        &sel_model, snum == 2),
                    wiz_field("3. Telegram Token",  &tgr,       snum == 3),
                    wiz_field("4. Discord Token",   &dr,        snum == 4),
                    Line::from(""),
                    Line::from(Span::styled(
                        if snum == 2 { "↑↓ select  Enter confirm".to_string() } else { format!("> {}_", active_val) },
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    )),
                ])
                .block(Block::default().title(" config.json + .env ").borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))),
                cols[0],
            );

            let list_items: Vec<ListItem> = mlr.iter().map(|m| ListItem::new(m.as_str())).collect();
            f.render_stateful_widget(
                List::new(list_items)
                    .block(Block::default().title(format!(" Models — {} ", msr)).borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
                    .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
                    .highlight_symbol("▶ "),
                cols[1],
                &mut mstate.clone(),
            );
        })?;

        if let Event::Key(k) = event::read()? {
            match (&step, k.code) {
                (WizStep::Url, KeyCode::Char(c))   => url.push(c),
                (WizStep::Url, KeyCode::Backspace)  => { url.pop(); }
                (WizStep::Url, KeyCode::Esc)        => done = true,
                (WizStep::Url, KeyCode::Enter) => {
                    if url.is_empty() { url = "http://localhost:11434".to_string(); }
                    term.draw(|f| {
                        f.render_widget(Paragraph::new(format!("  Connecting to {}…", url)).style(Style::default().fg(Color::Yellow)), f.area());
                    })?;
                    models = fetch_ollama_models(&url);
                    if models.is_empty() {
                        models = vec!["llama3.2".into(), "llama3".into(), "mistral".into(), "deepseek-r1:7b".into()];
                        mstatus = "⚠ Unreachable — pick a default".into();
                    } else {
                        mstatus = format!("✓ {} model(s) found", models.len());
                    }
                    mstate.select(Some(0));
                    step = WizStep::Model;
                }
                (WizStep::Model, KeyCode::Up)    => { let i = mstate.selected().unwrap_or(0); mstate.select(Some(i.saturating_sub(1))); }
                (WizStep::Model, KeyCode::Down)  => { let i = mstate.selected().unwrap_or(0); if i + 1 < models.len() { mstate.select(Some(i + 1)); } }
                (WizStep::Model, KeyCode::Enter) => step = WizStep::Tg,
                (WizStep::Model, KeyCode::Esc)   => done = true,
                (WizStep::Tg, KeyCode::Char(c))   => tg.push(c),
                (WizStep::Tg, KeyCode::Backspace)  => { tg.pop(); }
                (WizStep::Tg, KeyCode::Enter | KeyCode::Esc) => step = WizStep::Discord,
                (WizStep::Discord, KeyCode::Char(c))  => disc.push(c),
                (WizStep::Discord, KeyCode::Backspace) => { disc.pop(); }
                (WizStep::Discord, KeyCode::Enter | KeyCode::Esc) => done = true,
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;

    // Save config.json (no workspace mkdir here — that's main's job)
    let model = mstate.selected().and_then(|i| models.get(i)).cloned().unwrap_or_else(|| "llama3.2".to_string());
    AppConfig { ollama_base: url, model_name: model }.save()?;
    crate::config::Secrets { telegram_token: tg, discord_token: disc }.save()?;
    Ok(())
}

fn wiz_field(label: &str, value: &str, active: bool) -> Line<'static> {
    let pfx  = if active { "▶ " } else { "  " };
    let disp = if value.is_empty() { "—".to_string() } else { value.to_string() };
    Line::from(Span::styled(
        format!("{}{}: {}", pfx, label, disp),
        if active { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) }
        else      { Style::default().fg(Color::DarkGray) },
    ))
}
