# ShellTools

Owner: [shell_tools.rs](../../../../crates/dim-agent-capabilities/src/shell_tools.rs).
The Session part that declares the `execute` tool. It registers the tool with
`ToolRegistry`; the handler resolves the calling branch's `ShellSession` per
call, so the tool works in `shell` and `codemode` modes without a second
Session `ToolHost`.

## ShellTools

The part is mounted by the profile's ShellTools feature. The tool's risk is
`execute_process`; the mode projection hides `execute` outside `shell` and a
`codemode` program reaches it through `delegated`.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` | Register `execute` for the Session. |
| `P:HostPlatform` | The environment line in the tool description. |
| `S:TreeCommands` | Resolve the calling branch's `ShellSession`. |

### Tests

The minimal-execution integration suite runs `execute` through the tool
surface; the App Server's `session_tool_mode_flow` asserts the mode
projection.
