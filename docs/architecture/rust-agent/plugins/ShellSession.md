# ShellSession

Owner: [shell_session.rs](../../../../crates/dim-agent-capabilities/src/shell_session.rs).
The branch's persistent shell: one PTY per branch, started on its first
command, with cwd, environment, shell variables and jobs kept across calls.
It provides the branch's `ShellSession` and its `dyn ExecStop`, so the
Session's `execute` tool and the stop route reach it per branch.

## ShellSession

The branch part is mounted by the profile's ShellSession feature. It owns the
PTY, the command queue and the background jobs; `execute` resolves the calling
branch's service, and `session.exec.stop` asks the same branch's `ExecStop`
before falling back to `BackgroundExec`.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:BackgroundExec` | Recover a shell task whose facts were committed but whose owner is gone. |
| `B:Timeline` | Commit command starts, output and terminal ends. |
| `B:Dispatcher` | Deliver the completion notice and wake the Session. |
| `B:BranchLifecycle` | Wait for the branch to be published and stop the shell on closing. |
| `P:HostPlatform` | The shell executable and platform dialect. |
| `S:SessionConfig` | The Session's cwd and execution preferences. |
| `B:TimelineIdentity` | Identify the branch in its facts and notices. |

### Tests

`tests/integration/tests/minimal_execution.rs` covers the shell's persistence,
occupancy, background jobs, output bounds, stop and close. The App Server's
`transport_flow::shell_input` and `transport_flow::exec_shell_settings` cover
the product paths.
