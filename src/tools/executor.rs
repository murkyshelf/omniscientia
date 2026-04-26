use std::process::Command;
use std::fs;

/// A tool call parsed from the LLM response.
#[derive(Debug)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value,
}

pub struct Executor {
    pub access_level: u8,   // 0=Guest, 1=Employee, 2=Manager, 3=Admin
}

impl Executor {
    pub fn new(access_level: u8) -> Self {
        Self { access_level }
    }

    /// Parse the first `tool_call` JSON block from an LLM response string.
    pub fn parse_tool_call(response: &str) -> Option<ToolCall> {
        // Look for ```tool_call\n{...}\n```
        let start_tag = "```tool_call";
        let end_tag   = "```";
        let start = response.find(start_tag)? + start_tag.len();
        let rest  = &response[start..];
        let end   = rest.find(end_tag)?;
        let json_str = rest[..end].trim();
        let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
        let tool = value["tool"].as_str()?.to_string();
        Some(ToolCall { tool, args: value["args"].clone() })
    }

    /// Execute a parsed ToolCall and return the result string.
    pub fn execute(&self, call: &ToolCall) -> String {
        match call.tool.as_str() {
            "read_file" => self.read_file(&call.args),
            "write_file" => {
                if self.access_level < 3 {
                    return String::from("[DENIED] write_file requires Admin access.");
                }
                self.write_file(&call.args)
            }
            "shell_exec" => {
                if self.access_level < 2 {
                    return String::from("[DENIED] shell_exec requires Manager or Admin access.");
                }
                self.shell_exec(&call.args)
            }
            "python_exec" => {
                if self.access_level < 2 {
                    return String::from("[DENIED] python_exec requires Manager or Admin access.");
                }
                self.python_exec(&call.args)
            }
            "send_notification" => {
                if self.access_level < 2 {
                    return String::from("[DENIED] send_notification requires Manager or Admin access.");
                }
                self.send_notification(&call.args)
            }
            other => format!("[ERROR] Unknown tool: {}", other),
        }
    }

    // ── Tool implementations ─────────────────────────────────────────────────

    fn read_file(&self, args: &serde_json::Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return String::from("[ERROR] read_file requires 'path' argument."),
        };
        match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => format!("[ERROR] Could not read '{}': {}", path, e),
        }
    }

    fn write_file(&self, args: &serde_json::Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return String::from("[ERROR] write_file requires 'path' argument."),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return String::from("[ERROR] write_file requires 'content' argument."),
        };
        match fs::write(path, content) {
            Ok(_) => format!("Written {} bytes to '{}'.", content.len(), path),
            Err(e) => format!("[ERROR] Could not write '{}': {}", path, e),
        }
    }

    fn shell_exec(&self, args: &serde_json::Value) -> String {
        let command = match args["command"].as_str() {
            Some(c) => c,
            None => return String::from("[ERROR] shell_exec requires 'command' argument."),
        };
        let extra_args: Vec<&str> = args["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        match Command::new(command).args(&extra_args).output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            Ok(output) => {
                format!("[STDERR] {}", String::from_utf8_lossy(&output.stderr))
            }
            Err(e) => format!("[ERROR] Failed to run '{}': {}", command, e),
        }
    }

    fn python_exec(&self, args: &serde_json::Value) -> String {
        let script = match args["script"].as_str() {
            Some(s) => s,
            None => return String::from("[ERROR] python_exec requires 'script' argument."),
        };
        // Write to a temp file and run
        let tmp = "/tmp/_omniscientia_worker.py";
        if let Err(e) = fs::write(tmp, script) {
            return format!("[ERROR] Could not write temp script: {}", e);
        }
        match Command::new("python3").arg(tmp).output() {
            Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
            Err(e) => format!("[ERROR] python3 failed: {}", e),
        }
    }

    fn send_notification(&self, args: &serde_json::Value) -> String {
        let channel = args["channel"].as_str().unwrap_or("unknown");
        let user    = args["user"].as_str().unwrap_or("unknown");
        let message = args["message"].as_str().unwrap_or("(no message)");
        // Stub: In V2, this routes to a real channel implementation via src/channels/
        format!("[NOTIFICATION] → {} on {} channel: \"{}\"", user, channel, message)
    }
}
