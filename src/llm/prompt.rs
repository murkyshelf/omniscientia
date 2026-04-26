/// Builds the system prompt by reading memory files and assembling a rich context block.
use std::fs;
use std::path::Path;

/// All memory files under the `memory/` directory that should be loaded.
const MEMORY_FILES: &[&str] = &[
    "memory/SOLE.md",
    "memory/skills.md",
    "memory/tools.md",
];

/// Read a single memory file. Returns `None` if the file doesn't exist.
fn read_memory_file<P: AsRef<Path>>(path: P) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Assembles the complete system prompt sent to the LLM on every turn.
///
/// Structure:
///   1. Agent identity + personality (from SOLE.md)
///   2. Skills + capabilities (from skills.md)
///   3. Tool call format + available tools (from tools.md)
///   4. Current session metadata (user alias, role label)
///   5. Any extra context passed in (e.g. calendar, company records)
pub fn build_system_prompt(user_alias: &str, role_label: &str, extra_context: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Header
    parts.push(format!(
        "# Omniscientia — System Context\n\
        Current session: **{}** (Role: **{}**)\n",
        user_alias, role_label
    ));

    // Load memory files
    for &file in MEMORY_FILES {
        if let Some(content) = read_memory_file(file) {
            parts.push(format!("---\n{}", content.trim()));
        }
    }

    // Any extra context (e.g., company records pulled from DB)
    if let Some(ctx) = extra_context {
        if !ctx.is_empty() {
            parts.push(format!("---\n## Additional Context\n{}", ctx.trim()));
        }
    }

    parts.push(String::from("---\nRespond concisely. \
        Use `tool_call` JSON blocks (defined in tools.md) whenever a task requires action. \
        Do not describe what you would do — just do it."));

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_without_panic() {
        let prompt = build_system_prompt("Admin", "Admin", None);
        assert!(prompt.contains("Omniscientia"));
    }
}
