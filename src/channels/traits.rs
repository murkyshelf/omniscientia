use async_trait::async_trait;
use std::error::Error;

#[async_trait]
pub trait Channel {
    /// Identifier for the channel (e.g., "Telegram", "Discord")
    fn name(&self) -> &str;

    /// Dispatch a message to an external system (e.g., proactive Notification to an Employee)
    async fn send_message(&self, target_user: &str, content: &str) -> Result<(), Box<dyn Error>>;

    /// Receive a message from an external system
    async fn receive_message(&self) -> Result<String, Box<dyn Error>>;
}
