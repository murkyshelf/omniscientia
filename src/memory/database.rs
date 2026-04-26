use rusqlite::{Connection, Result, params};
use std::path::Path;

pub struct Database {
    conn: Connection,
}

#[derive(Debug)]
pub struct DbMessage {
    pub id: i64,
    pub user_id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct PendingUser {
    pub id:              i64,
    pub username:        String,
    pub real_name:       String,
    pub email:           String,
    pub tech_stack:      String,
    pub role_requested:  String,
    pub channel:         String,
    pub channel_user_id: String,
    pub status:          String,
    pub created_at:      String,
}

impl Database {
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let mut db = Database { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;

        // RBAC Roles
        tx.execute(
            "CREATE TABLE IF NOT EXISTS roles (
                id INTEGER PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                access_level INTEGER NOT NULL
            )",
            [],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO roles (id, name, access_level) VALUES
             (1, 'Guest', 0), (2, 'Employee', 1), (3, 'Manager', 2), (4, 'Admin', 3)",
            [],
        )?;

        // Users & Analytics
        tx.execute(
            "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                role_id INTEGER NOT NULL REFERENCES roles(id),
                personality_profile TEXT DEFAULT '{}',
                productivity_score REAL DEFAULT 0.0
            )",
            [],
        )?;

        // Conversation History
        tx.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY,
                user_id INTEGER NOT NULL REFERENCES users(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        // Company Records
        tx.execute(
            "CREATE TABLE IF NOT EXISTS company_records (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                min_access_level INTEGER NOT NULL
            )",
            [],
        )?;

        // Pending users from bot channels
        tx.execute(
            "CREATE TABLE IF NOT EXISTS pending_users (
                id INTEGER PRIMARY KEY,
                username TEXT NOT NULL,
                real_name TEXT DEFAULT '',
                email TEXT DEFAULT '',
                tech_stack TEXT DEFAULT '',
                role_requested TEXT DEFAULT 'Employee',
                channel TEXT NOT NULL,
                channel_user_id TEXT NOT NULL,
                status TEXT DEFAULT 'pending',
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(channel, channel_user_id)
            )",
            [],
        )?;

        // Onboarding session state — persisted so server restarts don't repeat questions
        tx.execute(
            "CREATE TABLE IF NOT EXISTS onboarding_sessions (
                chat_id        TEXT PRIMARY KEY,
                channel        TEXT NOT NULL,
                step           INTEGER DEFAULT 0,
                tg_username    TEXT DEFAULT '',
                real_name      TEXT DEFAULT '',
                email          TEXT DEFAULT '',
                tech_stack     TEXT DEFAULT '',
                role_requested TEXT DEFAULT '',
                updated_at     DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        tx.commit()?;
        Ok(())
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    pub fn upsert_user(&self, username: &str, role_id: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO users (username, role_id) VALUES (?1, ?2)",
            params![username, role_id],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM users WHERE username = ?1",
            params![username],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    // ── Messages ──────────────────────────────────────────────────────────────

    pub fn save_message(&self, user_id: i64, role: &str, content: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (user_id, role, content) VALUES (?1, ?2, ?3)",
            params![user_id, role, content],
        )?;
        Ok(())
    }

    pub fn load_recent_messages(&self, user_id: i64, limit: u32) -> Result<Vec<DbMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, role, content, timestamp FROM messages
             WHERE user_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![user_id, limit], |row| {
            Ok(DbMessage {
                id: row.get(0)?,
                user_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                timestamp: row.get(4)?,
            })
        })?;
        let mut msgs: Vec<DbMessage> = rows.filter_map(|r| r.ok()).collect();
        msgs.reverse();
        Ok(msgs)
    }

    pub fn list_usernames(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT username FROM users ORDER BY username")?;
        let names = stmt.query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(names)
    }

    // ── Pending users (pairing) ───────────────────────────────────────────────

    pub fn add_pending_user(&self, username: &str, channel: &str, channel_user_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pending_users (username, channel, channel_user_id)
             VALUES (?1, ?2, ?3)",
            params![username, channel, channel_user_id],
        )?;
        Ok(())
    }

    /// Store a fully-completed onboarding entry (all fields from the bot flow).
    pub fn add_pending_user_full(
        &self, username: &str, real_name: &str, email: &str,
        tech_stack: &str, role_requested: &str,
        channel: &str, channel_user_id: &str,
    ) -> Result<()> {
        // INSERT OR IGNORE: if (channel, channel_user_id) already exists, do nothing
        self.conn.execute(
            "INSERT OR IGNORE INTO pending_users
             (username, real_name, email, tech_stack, role_requested, channel, channel_user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![username, real_name, email, tech_stack, role_requested, channel, channel_user_id],
        )?;
        Ok(())
    }

    // ── Onboarding sessions ───────────────────────────────────────────────────

    pub fn load_onboard_session(&self, chat_id: &str) -> Result<Option<OnboardSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT chat_id, channel, step, tg_username, real_name, email, tech_stack, role_requested
             FROM onboarding_sessions WHERE chat_id = ?1"
        )?;
        let mut rows = stmt.query_map(params![chat_id], |row| {
            Ok(OnboardSession {
                chat_id:       row.get(0)?,
                channel:       row.get(1)?,
                step:          row.get(2)?,
                tg_username:   row.get(3)?,
                real_name:     row.get(4)?,
                email:         row.get(5)?,
                tech_stack:    row.get(6)?,
                role_requested: row.get(7)?,
            })
        })?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    pub fn save_onboard_session(&self, s: &OnboardSession) -> Result<()> {
        self.conn.execute(
            "INSERT INTO onboarding_sessions
             (chat_id, channel, step, tg_username, real_name, email, tech_stack, role_requested)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(chat_id) DO UPDATE SET
               step = excluded.step, tg_username = excluded.tg_username,
               real_name = excluded.real_name, email = excluded.email,
               tech_stack = excluded.tech_stack, role_requested = excluded.role_requested,
               updated_at = CURRENT_TIMESTAMP",
            params![s.chat_id, s.channel, s.step, s.tg_username,
                    s.real_name, s.email, s.tech_stack, s.role_requested],
        )?;
        Ok(())
    }

    pub fn delete_onboard_session(&self, chat_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM onboarding_sessions WHERE chat_id = ?1", params![chat_id])?;
        Ok(())
    }

    pub fn update_pending_user_status(&self, id: i64, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE pending_users SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        Ok(())
    }

    pub fn list_pending_users(&self, status_filter: Option<&str>) -> Result<Vec<PendingUser>> {
        let sql = match status_filter {
            Some(_) => "SELECT id,username,real_name,email,tech_stack,role_requested,\
                        channel,channel_user_id,status,created_at \
                        FROM pending_users WHERE status = ?1 ORDER BY created_at DESC",
            None    => "SELECT id,username,real_name,email,tech_stack,role_requested,\
                        channel,channel_user_id,status,created_at \
                        FROM pending_users ORDER BY created_at DESC",
        };

        let mut stmt = self.conn.prepare(sql)?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<PendingUser> {
            Ok(PendingUser {
                id:              row.get(0)?,
                username:        row.get(1)?,
                real_name:       row.get(2)?,
                email:           row.get(3)?,
                tech_stack:      row.get(4)?,
                role_requested:  row.get(5)?,
                channel:         row.get(6)?,
                channel_user_id: row.get(7)?,
                status:          row.get(8)?,
                created_at:      row.get(9)?,
            })
        };

        let rows = if let Some(sf) = status_filter {
            stmt.query_map(params![sf], map_row)?
        } else {
            stmt.query_map([], map_row)?
        };

        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

// ─── Onboarding session ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct OnboardSession {
    pub chat_id:        String,
    pub channel:        String,
    pub step:           i32,
    pub tg_username:    String,
    pub real_name:      String,
    pub email:          String,
    pub tech_stack:     String,
    pub role_requested: String,
}
