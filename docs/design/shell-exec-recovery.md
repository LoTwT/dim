# Shell exec recovery

ShellSession owns its branch's persistent shell, command queue and foreground-to-background handoff.
It does not write exec facts: a background job and a foreground command that outlives its wait are
handed to BackgroundExec, which registers the task, publishes its terminal and settles it.
BackgroundExec owns recovery and completion notices for that shared exec protocol; its
[query, fork boundary and notice contract](../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)
applies to facts from either execution producer. Event types do not have an exclusive writer.

## Assembly and handoff

The minimal-execution profile mounts BackgroundExec alongside ShellSession. ShellSession declares a
required dependency on `backgroundExec`: recovery finishes before a new shell can accept commands,
and a reload of the recovery owner first unloads its shell dependent. A ShellSession-only reload does
not restart the recovery owner or mistake another producer's live work for abandoned history.
The local-execution profile already provides BackgroundExec, so a host adding ShellSession there reuses it.

ShellSession hands a task to BackgroundExec only after HostPlatform accepted the command or job; on
handoff failure BackgroundExec terminates and settles the owner task. HostPlatform records every
shell task's terminal from its markers and sweeps a dead shell's commands and jobs to their
expected terminals. BackgroundExec then writes the one `exec/ended` per task and posts the wake
input; nobody synthesizes a result or publishes one because a wait timed out. The only stop entry is
`BackgroundExec.stop`; there is no separate shell stop port.

## Recovery cases

| Committed prefix at restoration | Required outcome |
| --- | --- |
| Started, no ended | One failed terminal stating that the old execution result is unknown; no command restart |
| Eligible ended, no tagged notice | Keep the terminal and post the missing notice |
| Ended and tagged notice | Neither a second terminal nor a second notice |
| Cancelled terminal | No completion wake |
| Facts inherited across the Session fork boundary | No adoption of copied external work |

Recovery uses BackgroundExec's structured selection at a fixed cutoff. It does not replay command
execution, scan completed history into a cache, or change the persistent event format.
Publication, failure handling and retry on the next generation remain the owner's existing contract.

## Rule check

Both branch parts are Capabilities under principles §1. ShellSession owns I/O policy and the command
queue; BackgroundExec owns registration, terminal facts, recovery and notification for every task it
registers. This changes who registers a shell task, not the meaning of an exec event.

- Principles §4 rules 1, 2 and 7: the owner terminal is the handoff; neither part mutates the other's state.
- Rule 3: no before callback.
- Rules 4 and 8: existing Timeline queries, Dispatcher and dependency lifecycle; no mechanism change.
- Rule 5: existing separate terminal/notice commits remain valid, including recovery of that gap.
- Rule 6: notices use the existing input adoption path.
- Rule 9: no new event or UI fact; every retained fact drives a later recovery or notification decision.
- ADR 002 D1–D3: existing branch owners and a declared load-order dependency; no wiring part or core dependency on ShellSession.
- D4: no new setting. D5: no new service method, wire field or binding protocol.

## Requirements raised to mechanism owners

None. Existing query, publication and lifecycle contracts suffice.

## Verification

The minimal-execution flow restores disposable storage containing an unfinished start, an unreported
completed end and a cancelled end. It checks failed settlement, preservation of the existing result,
notice eligibility, each notice's stored command description and no duplicates on another restoration. These are committed-prefix recovery
cases, not a claim that the test killed a real host process. Live background and occupying-command flows also check the completion notice.
A shell-exit flow checks the terminal the owner records and that the swept job's process is gone; the
close flow checks one terminal per admitted task, and a recovery restart checks that the live shell
task settles before recovery inspects history.
