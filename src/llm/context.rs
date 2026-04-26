//! Phase 3 — Rolling Context Manager
//!
//! When a user's message count exceeds `MAX_TURNS`, the oldest batch is
//! summarised via the LLM and stored as `role="summary"` in the DB.
//! On each chat call, the context window is built as:
//!   [MEMORY SUMMARY] (if any) + last MAX_RECENT turns + system prompt

use crate::memory::database::{Database, DbMessage};
use crate::llm::provider::LlmProvider;

const MAX_TURNS:   usize = 30;   // summarise if history > this
const KEEP_RECENT: usize = 10;   // keep this many fresh turns after summarising
const MAX_RECENT:  usize = 20;   // cap for normal chat context

pub struct ContextManager<'a> {
    db:       &'a Database,
    provider: &'a LlmProvider,
    user_id:  i64,
}

impl<'a> ContextManager<'a> {
    pub fn new(db: &'a Database, provider: &'a LlmProvider, user_id: i64) -> Self {
        Self { db, provider, user_id }
    }

    /// Returns the context to pass to the LLM as `(system_prefix, messages)`.
    /// Automatically triggers summarisation if the history is too long.
    pub async fn get_context(&self, system_base: &str) -> (String, Vec<(String, String)>) {
        let all = self.db.load_all_messages(self.user_id).unwrap_or_default();

        // Split: summaries vs. turn messages
        let summaries: Vec<_> = all.iter().filter(|m| m.role == "summary").collect();
        let turns: Vec<_>     = all.iter().filter(|m| m.role != "summary").collect();

        // Trigger rolling summarisation if turns exceed threshold
        if turns.len() > MAX_TURNS {
            self.summarise_old(&turns).await;
            // Recurse once to pick up the new summary
            return self.get_context(system_base).await;
        }

        // Build system prefix from the latest summary (if any)
        let summary_text = summaries.last().map(|s| s.content.as_str()).unwrap_or("");
        let system = if summary_text.is_empty() {
            system_base.to_string()
        } else {
            format!("{}\n\n[MEMORY SUMMARY]\n{}", system_base, summary_text)
        };

        // Take the most recent MAX_RECENT turns only
        let recent = if turns.len() > MAX_RECENT {
            &turns[turns.len() - MAX_RECENT..]
        } else {
            &turns[..]
        };

        let messages: Vec<(String, String)> = recent.iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();

        (system, messages)
    }

    /// Summarise the oldest `(turns.len() - KEEP_RECENT)` turns and store the
    /// summary as a `role="summary"` message.
    async fn summarise_old(&self, turns: &[&DbMessage]) {
        let n_to_summarise = turns.len().saturating_sub(KEEP_RECENT);
        if n_to_summarise == 0 { return; }

        let batch = &turns[..n_to_summarise];

        // Build a short conversation transcript for the LLM
        let transcript: String = batch.iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Summarise the following conversation concisely (key decisions, facts, user preferences). \
             Keep it under 200 words.\n\n---\n{}\n---",
            transcript
        );

        let sys = "You are a concise assistant. Produce a short factual summary only.";
        if let Ok(summary) = self.provider.chat_with_context(sys, &[], &prompt).await {
            let _ = self.db.save_message(self.user_id, "summary", &summary);
            // Mark the summarised turns as archived
            let _ = self.db.archive_messages_before(self.user_id, batch.last().map(|m| m.id).unwrap_or(0));
        }
    }
}
