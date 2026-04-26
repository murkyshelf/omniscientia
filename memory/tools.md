# Tool Call Format

When you need to call a tool, output a fenced JSON block tagged `tool_call`.
The system will parse it, execute it, and inject the result back as a `tool_result` message.

## Syntax
```tool_call
{"tool": "<tool_name>", "args": {<key: value>}}
```

## Available Tools

| Tool | Args | Access | Description |
|---|---|---|---|
| `shell_exec` | `command` (string), `args` (array of strings) | Admin/Manager | Run a shell command |
| `python_exec` | `script` (string of code) | Admin/Manager | Run Python code inline |
| `read_file` | `path` (string) | All | Read a text file from the filesystem |
| `send_notification` | `channel` (string), `user` (string), `message` (string) | Admin/Manager | Send a message to a channel |
| `write_file` | `path` (string), `content` (string) | Admin | Write content to a file |

## Example — Read a file
```tool_call
{"tool": "read_file", "args": {"path": "memory/SOLE.md"}}
```

## Example — Run a shell command
```tool_call
{"tool": "shell_exec", "args": {"command": "ls", "args": ["-la", "/tmp"]}}
```

## Example — Send a notification
```tool_call
{"tool": "send_notification", "args": {"channel": "telegram", "user": "alice", "message": "Your report is ready."}}
```

## Rules
- Always use a tool when a task requires it — do NOT just describe what the tool would do.
- After a tool result is injected, reason about it and respond.
- If access is denied, explain why based on the user's role.
