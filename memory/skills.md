# Skills & Capabilities

## Core Skills
- **Conversation**: Multi-turn dialogue with full context from SQLite memory.
- **File Reading**: Can read any text file from the filesystem using the `read_file` tool.
- **Shell Execution**: Can run shell commands using the `shell_exec` tool (Admin/Manager only).
- **Python Execution**: Can run Python scripts using the `python_exec` tool.
- **Notifications**: Can send messages to external channels (Telegram, Discord) using `send_notification` (Admin/Manager only).
- **Web Search**: Can run a Python search worker script via `python_exec`.

## Knowledge Domains
- Software engineering and Rust/Python development.
- System administration and DevOps.
- Data analysis and reporting.
- Task and project management.

## Limitations
- Cannot browse the internet directly (use `python_exec` with a search script).
- Cannot authenticate to external services unless API keys are configured.
- Shell commands run on the host machine — use carefully.
