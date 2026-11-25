# FileStorage

Owner: [file_storage.rs](../../../../crates/dim-agent-capabilities/src/file_storage.rs)
and [windows.rs](../../../../crates/dim-agent-capabilities/src/file_storage/windows.rs).
Provides authorized-root-relative file operations and leases with secure path
traversal and serialized mutation, plus directory creation, listing and removal.

## Dependencies

No injected services (`Deps = ()`).

## Events and lifetime

No Timeline events. Each `apply` opens fresh root-directory handles; blocking
operations and leases are tracked by that generation. Close stops admission,
drains operations/leases and releases resources. Retained old service handles
reject new operations. Clone the plugin to preserve append-only authorized-root
indices across reload; rebuilding from the original list could reuse indices.
Cancellation is checked before commit, not treated as rollback after installation.
Roots reject symlinked traversal; this is a trusted host storage boundary, not
isolation from the host or same-user processes. Do not automatically retry an
uncertain committed mutation. The native `open` used by each operation receives
exactly one already validated path component, never a multi-part path, so `..`
or an intermediate symlink cannot escape the held parent handle. Native Windows
opens leave `OBJECT_ATTRIBUTES.Attributes` at 0 so the actual directory's
case-sensitivity setting governs lookup; forcing `OBJ_CASE_INSENSITIVE` or
folding names would break case-sensitive directories and their aliases.

## Authorized roots

A root is its index and its path. Both are append-only: an index always names
the same path, and a `FilePath` names its root for its generation's lifetime. A
descriptor is not part of that identity. Registering an already registered path
opens the directory the path names now and keeps the index: the root keeps
serving the same directory when the path still names it, serves the new one when
the path names another, and holds none when the path cannot be opened. Reading
the directory's identity can fail, on a volume that does not report file IDs or
while a deletion is pending; that comparison failing is not a registration
failure, and the newly opened descriptor, whose component-wise walk already
rejected links, serves the path.
The root records no directory for that observation, so a later registration
cannot mistake the path for one the root served before the failed read.

Every observation of one root is serialized with the slot transition it
publishes, so an observation never overtakes or is overtaken by another. A
normal operation uses the descriptor the root last held; it does not check the
path's current identity. A directory another process replaces at the path is
therefore reached when that path is registered again or after a release, not by
every operation on the root.

`release_root(index)` drops the descriptor and keeps the index, its path and
every `FilePath` naming it. The next operation opens the directory the path
names then, so a release withdraws no authorization and changes no index. A
released root costs no descriptor until then, and an open that began before a
release does not put its descriptor back.

A root's slot is Open or Unavailable, at a generation. A lease is bound to the
generation it locked: `replace` and `backup` through it fail with `invalid`
unless the slot is Open at that generation, so a lease never writes into a
directory the path no longer names. The generation advances when an observation
reads an identity for the path that differs from the one the root serves, and
when it reads one after none was recorded: the root cannot show that the
directory is one it already served. A release, which names the same directory,
leaves the lease valid, and so does an identity that cannot be read while the
slot is Open: the storage cannot tell whether the path names another directory.
A slot an observation could not open refuses its leases' mutations. An
observation that opens the path and reads the directory the root served before
it restores them; one that opens another directory, or cannot read one, starts a
generation, so only leases locked through that open are admitted. A release does
not clear that state because it reads no path. Their reads keep the files they
pinned, and dropping them releases their locks. An open that a release overtook
keeps no descriptor for the root, because the release dropped it, and the
observation serves that operation alone.

The App Server releases a project's root once the last Session loaded from its
cwd settles. `session.close`, `session.delete`, a Collaboration member's close,
an idle unload and `agent.close` each end one Session's ownership; a load
records it before the Session appears in an Agent's Session list, so a Session
still restoring owns the root. A restore that loaded no Session ends the
ownership it registered itself, before its caller can be told either way, so a
caller that stopped waiting leaves none behind. A loaded Session's ownership
ends with its close, which the process observes even when it did not start the
close, so a Session the runtime's fault supervisor stops releases its project
too. A guard settles only the ownership its own load registered: the idle pass
and a load's guard watch one close, and a guard that reaches it after a later
load of the same Session ID registered a new ownership leaves that ownership
and the root it holds. `agent.close` ends every ownership the Agent registered,
including a Session whose build reached the runtime after the close began, and
an Agent scope that unloads ends the ownerships of the Sessions its own close
ended. A `session.detach` ends no ownership: the Session stays loaded and may
still use the project, and its later close or unload releases the root. An
authorization no Session used, from `hooks.catalog` or MCP management, owns
nothing and keeps its descriptor until the next settlement on the same cwd or
the process closes.
FileStorage close still releases every descriptor. A reopened descriptor is not
revalidated against an earlier one, because the path is the authorization and
the authorization did not change.

## Bounded reads

`read_limited(path, limit, cancel)` preserves the same authorized-handle traversal and cancellation as `read`. It checks the opened file's length before allocating contents, then checks each chunk before extending the buffer to cover concurrent growth. Exceeding the byte limit returns `Io(InvalidData)` without partial bytes or a write. The lease's own `read_limited(limit, cancel)` applies the same rule under the held lock; `read` is its unbounded case. The business caller chooses its byte limit.

## Filesystem data face

`fsHost.createDirectory`, `list` and `remove` resolve absolute paths in the owner's
authorized roots. They use the directory creation, listing and removal contracts above.
Pass a listed name unchanged to `remove({parent, name})`. A Unicode name is a string. Other names carry the original Unix bytes (`{Unix: number[]}`) or Windows UTF-16 code units (`{Windows: number[]}`). The owner accepts a single component below the listed directory.

`fsHost.stat({path})` returns the entry kind and the size of a regular file, without following links. An absent entry has kind `missing`.

`fsHost.readBytes({path, limit, leaseId?})` returns base64 bytes and the length. A file over the limit fails with `too_large`. Hooks reads the authorized project settings path through this operation. `lock({path})` returns a lease ID for reads of that same file. `replace({path, bytes, permissions?, leaseId?})` accepts base64 bytes and atomically replaces the target under the supplied lease or a new lock. `backup({leaseId, suffix, permissions?})` creates an exclusive sibling copy under the same lease. Explicit `unlock({leaseId})` or caller unload releases the lease.

### Pinned reads

The process `fsHost` name provides the pinned-reader operations from
[resource faces §4.1](../../../design/resource-faces.md#41-methods), using this
same FileStorage generation and reader registry. `openRead` resolves the supplied
absolute path through the existing authorized-root traversal and pins that exact
file. Replacing the path afterwards does not change the reader's bytes.

The wire `openRead` operation requires a calling row to own retention; `readAt`
and `closeRead` require only the reader id. These are trusted-plugin operations:
an id identifies a reader, not caller authorization, and a plugin that receives
another row's id can read or close it. Readers opened through native
`FileStorage::open_read` have no caller retention to release through this face; attempting to close such an id returns `invalid`
without unregistering it.

`readAt` keeps the existing maximum of 64 KiB and independent offsets. Zero limit
or an offset at/past the pinned length returns empty bytes. The wire encodes bytes
as base64; the native binding reads the same reader id directly. Unknown, closed
or prior-generation ids are `invalid`; unavailable storage is `node_unavailable`,
a missing file is `not_found`, cancellation is `cancelled`, and other existing
I/O failures are `io`. No extra read or normalization follows the owner's result.

The calling row retains an opened reader until `closeRead` or that row's unload;
registration finishes before `openRead` returns. Close removes the id before
releasing retention, while reads already accepted finish under the owner's
existing operation tracking. Dropping close before it is polled does nothing;
once close removes the id, the existing context cleanup continues even if its
wait is dropped. FileStorage close drains those operations and
releases the same pinned handles, including readers retained by a caller. There
is one reader registry and no separate connection table or read queue.

In the Rust binding, `FsHost::open_read` takes the calling part's `&Ctx` and
cancellation token along with the path. The context supplies the same cleanup
registration that the wire binding obtains from its calling row; it is not a
request field. Both return only `OpenRead {read_id, size}`. The public `FsHost`
binding has these three operations, no owner dereference or native file handle;
its methods directly return the existing owner's operation futures. Native
FileStorage readers outside this face retain their original weak-registry/RAII
lifetime while sharing the same generation's shutdown.

## Leases and lock keys

A lease locks a key file with the target's name in the reserved
`.dim-file-storage-locks` directory beside the target, so the filesystem's own
case and Unicode aliases of one name share one key. Releasing a lease keeps its
key; only directory removal unlinks keys, one at a time while holding each. A
lock counts only if its key is still linked once held. A key linked when locked
stays linked while held, because only its holder unlinks it. A key unlinked
before it was held is stale: the current key is resolved once more, and a
second stale key returns `Locked`. Without that check, a lock racing a failed
removal admitted a second holder:

```text
B: lock(d/x)              remove(d)                    C: lock(d/x)
open key x (inode 1)
                          lock, unlink every key,
                          rmdir the lock directory
                                                       recreate the lock directory,
                                                       create and lock key x (inode 2)
                          rmdir d fails, release keys
lock inode 1              -> before: B and C both hold d/x
                          -> now: inode 1 is unlinked, so B reopens key x,
                             finds inode 2 held and returns Locked
```

Creating a nonexclusive key creates it once and then opens that inode:
concurrent creation can return `ENOENT` on macOS even after the winner
created the key.

A replacement writes its temporary file in
`.dim-file-storage-locks/.dim-file-storage-temps` under its key's name, so only
that key's holder writes or removes it. A leftover from an interrupted
replacement is discarded by the next replacement of that name. `replace_from`
takes the replacement from a reader one 64 KiB chunk at a time with the same
temporary file, permissions, rename and durability rules, so a replacement
need not be held whole; a read that fails or is cancelled before the rename
removes the temporary file and leaves the target as it was.

`auth.json` also takes the TypeScript-compatible `auth.json.lock` marker, which
is reclaimed only while holding the `auth.json` key. A marker is stale once it
is older than 30 seconds and has no verified owner: its recorded PID must still
be running and have started no later than the marker's recorded creation time.
A reused PID, a permission-denied process probe or a missing PID or creation
time does not establish ownership. The exception is a marker this
process failed to release: it keeps the owner record, whose nonce only that
marker carries, and reclaims the marker at once, so removal and locking need
not wait for the process to exit. The marker is the external TypeScript wire
format: PID, milliseconds since the Unix epoch (`Date.now()` in TypeScript),
nonce and a trailing newline, with no other fields. Writers must use
milliseconds, not seconds.

The start-time comparison on macOS, Linux, Android and Windows assumes wall-clock
adjustments do not make the reported process start later than marker creation;
otherwise a live owner's marker can be reclaimed after the grace period. Other
Unix targets retain the marker conservatively while its PID probe succeeds.

On Windows, releasing a `PhysicalLock` unlocks its byte range explicitly before
release completes, because `CloseHandle` may defer the implicit unlock; a later
generation would otherwise see a stale `Locked` lock.

## Directory creation

`create_directory` makes a path below an authorized root an existing business
directory, one level at a time: its parent must exist and is reached as every
operation reaches it, without following links. A missing directory is created
relative to the held parent handle, with mode `0700` on Unix (`mkdirat`) and
the parent's inherited DACL on Windows (`NtCreateFile` with `FILE_CREATE`). An
existing business directory counts as created, so a retry is safe; any other
existing entry (a file, link, reparse point or special file) fails with an I/O
error and is left untouched.

Aliases are checked the way `lock` checks them, before anything is created. The
parent's reserved directory is ensured first, so a name the filesystem equates
with it is recognized by identity. Then the key for `auth.json.lock` is ensured
and the key for the name is opened if it exists; if they are the same file, the
name is an alias of the account marker. Both fail `InvalidPath`. The name's key
is never created, so making a directory leaves no key behind in a parent that
may never be removed, such as SessionStore's Session-files directory.

```text
create_directory(d/x):
  open d (no links, never the reserved directory)       missing -> NotFound
  ensure d/.dim-file-storage-locks
  x is the reserved directory under another spelling?    -> InvalidPath
  ensure key auth.json.lock; key x exists and is it?     -> InvalidPath
  cancelled?                                             -> Cancelled
  mkdirat(d, x, 0700); an existing entry is kept
  x is not a business directory?                         -> Io, untouched
  fsync d                                                fails -> Unsynced
```

In both the created and the existing case the parent is synced before the call
returns, so a retry after `Unsynced` confirms durability. Windows has no
directory sync, as for replacement. Cancellation is checked up to the creation.

`create_directories` makes a multi-level path an existing directory tree by
calling that same one-level creation for each component, holding the opened
parent between levels, and returns the absolute path the path names below the
root. A component that already exists as a link, file or other non-directory
fails `InvalidPath` and is left untouched, and so does an alias of a reserved
resource; the returned path therefore names the result without a followed
link. This is the primitive for handing a work directory to a helper process.

## Listing and removal

`list` returns the entries of a directory below an authorized root, sorted by
name, each a `File`, `Directory` or `Other` (a link, reparse point or special
file, never followed). FileStorage's lock, temporary and account-marker
resources are omitted, including names the filesystem equates with them.

`EntryKind` serializes as `file`, `directory` or `other` for generated exports.
Native entry names remain `OsString`; the exporting boundary chooses its UTF-8
policy, and one that returns a path-shaped name joins the resolved components
with `/`, so the name round-trips through its own path resolution: on Unix a
backslash is part of a name, and on Windows it is a separator.

`remove` makes an entry absent. Files, including those beneath a directory,
are removed under the lock leases use, so removing a leased file, or a directory
with a lease held anywhere beneath it, returns `Locked`. A link is removed
itself. A directory is emptied depth first through directory handles, without
following links or entering a filesystem mounted at or inside it (a Linux bind
mount of the same filesystem is not detected). An explicit stack replaces
recursion, so depth costs no thread stack. On Unix, removal holds one directory
descriptor at a time: it returns to each parent through `..` and fails if that
is no longer the directory it descended from, or starts again from the target if
a concurrent removal deleted it. Windows holds a handle per level, since it
cannot reopen a parent from a handle without a path.

Before emptying a directory, removal clears the leftovers that would stop
every file in it from being locked: a non-directory in place of the lock
directory or of the temporary directory inside it, and in the lock directory
any link, or any directory other than the temporary one, such as a directory
in place of a key. Keys are regular files and no `FilePath` reaches inside the lock
directory, so no lease holds these. The one exception is a regular file in
place of the temporary directory, which can be a key, so it goes under its
lock. Trees there are removed without locks, entering directories with
reserved names as well, but never another filesystem. The temporary
directory, however it is spelled, is never taken for such a tree: that is
checked on the handle the removal walks, so a temporary directory created
meanwhile is not either.

Then the directory's other leftovers go first: entries with a reserved name,
or with the identity of the lock directory or the account marker but another
type. Examples are a resource from an earlier version and a marker that is a
directory or a link. They follow the business rules, since on a
case-insensitive volume an alias of a reserved name can be leased. Going first
means a stray marker cannot block locking `auth.json` beside it. A file whose
key the filesystem equates with the temporary directory, such as a file named
`.dim-file-storage-temps`, or `.DIM-FILE-STORAGE-TEMPS` on a case-insensitive
volume, cannot be locked, so no lease holds it; it is unlinked without a lock
unless it is the account marker under another name. The directory's FileStorage resources go last and hold one
key at a time, however many names the directory has used:

```text
remove(d), per directory, before its entries:
  a non-directory in place of .dim-file-storage-locks  -> unlink it
  in .dim-file-storage-locks:
    a file in place of temps                           -> lock it as a key, unlink
    any link                                           -> unlink it
    any directory but temps                            -> remove it as a tree
remove(d), once d's other entries are gone:
  auth.json.lock: holding key auth.json, reclaim it if stale; live -> Locked
  for each key k in .dim-file-storage-locks:
    lock k                                             held -> Locked
    k unlinked by a concurrent removal?                -> skip it
    remove temps/k (a directory as a tree), unlink k, release k
  for each temps/t still left (written under a key taken since,
  or named at random by an earlier version): the same, under key t,
  or without a key if key t would be temps itself
  rmdir temps, rmdir .dim-file-storage-locks, rmdir d
                    an entry or key created meanwhile -> DirectoryNotEmpty
remove(p), once p is absent (also when it already was):
  fsync p's parent                                     fails -> Unsynced
```

A link or special file is unlinked only while it still is one. If a key or
file has replaced it since it was listed, removal fails instead. Windows
checks the type through the handle that deletes the entry; Unix checks it
just before unlinking, which narrows the race without closing it.

Locking and replacement never delete leftovers. A non-directory in place of
the lock or temporary directory, or a directory in place of a key or a
temporary file, makes `lock` or `replace` fail with an I/O error, without
following or writing through it, until the directory is removed.

On Windows the replacement's temporary file is created with the target ACL in
the create call, never under the maintenance directory's inherited ACL and
tightened afterwards. Native replacement does not merge alternate data streams,
so a target with a non-default stream fails explicitly. Its rename uses
`NtSetInformationFile` with the held parent handle and a basename, without
resolving an absolute path again, so a parent or symlink replaced after
validation cannot be followed.

Removal therefore never unlinks a key or temporary file that a holder has
locked. An absent entry counts as removed. Removal is not atomic: an error or
cancellation can leave part of a tree, retrying is safe, and an owner that must
not leak files after a crash reconciles by listing. Once the entry is absent,
removal syncs its parent before it returns, also when the entry was already
absent, so a completed removal survives a crash and a retry after `Unsynced`
confirms durability; an owner can order a commit after it, as SessionStore's
Session delete does ([S5c](../../../design/appserver/session-deletion-and-fork.md#4-crash-convergence)).
Windows has no directory sync, as for replacement. The
owner orders removal after the directory's writers; entries or leases created
meanwhile make it fail. A replacement racing the removal of its directory still
commits: if removal deleted the empty temporary directory first, the
replacement creates it again. Roots, the reserved directory and the account
marker cannot be listed or removed.

On Windows, every reparse point is listed as `Other` and never entered. Removal
deletes a junction or symbolic link itself, but refuses a volume mount point
(a mount-point reparse point whose target is a `\??\Volume{…}` path) with
`CrossesDevices`, as Unix refuses a directory on another filesystem; Unix lists
such a directory as `Directory`. Listing and removal need a volume that reports
file IDs during enumeration, such as NTFS or ReFS. Elsewhere, such as on FAT,
exFAT or some network shares, they fail with an `Unsupported` I/O error rather
than list entries whose aliases of reserved names could not be recognized.

A directory deleted while an operation runs inside it has the same effect on
both platforms. Unix fails an operation in a deleted directory, or on a deleted
entry, with not found. Windows fails it with access denied, as it does a real
denial (`STATUS_DELETE_PENDING` or `STATUS_FILE_DELETED`). FileStorage then
checks whether the handle's directory or entry is pending deletion and, if
so, reports not found. A real denial in a live directory stays an error. A
file deleted while a lock resolves its stored name counts as missing, rather
than taking the name NTFS gives deleted files, and a key that fails to lock
once a removal unlinked it is stale on either platform. A lock racing the
removal of its directory therefore resolves its key again or returns
`Locked`, or not found once the directory is gone. A removal racing
another removal of the same tree treats what the other deleted as removed,
and starts again from its target if a directory it returns to was deleted.

## Tests

[file_storage_flow.rs](../../../../crates/dim-agent-capabilities/tests/file_storage_flow.rs):
`late_roots_preserve_indices_and_generation_boundaries_on_reload`,
`a_recreated_directory_at_a_registered_path_is_read_through_it` (a directory
replaced at a registered path keeps the index and the new directory serves the
same `FilePath`), `a_released_root_opens_the_directory_its_path_names` (the
released descriptor is closed, and the next operation opens the directory the
path names then), `a_lease_on_a_replaced_root_refuses_its_next_replacement` (a
lease locked in the old directory fails its next replacement and leaves the new
directory untouched), `a_lease_survives_a_root_release`,
`file_storage_flow` (path, lock, permission and platform checks),
`directory_creation_stays_inside_the_authorized_root` (mode, an existing
directory, a missing parent, aliases of the reserved directory and the account
marker, an existing file or link, cancellation),
`streaming_replacement_and_nested_directories_stay_inside_the_authorized_root`
(replacement from a reader with a bounded read, a source that cancels part-way
leaving the old contents and no temporary file, nested creation with modes, an
existing directory, and a file or link at any level),
`directory_listing_and_removal_stay_inside_the_authorized_root` (Session-file
layout, reserved resources and aliases, leases held during partial removal,
links, cancellation, leftovers with reserved names or of the wrong type, a
leased alias of a reserved name, stray entries in the lock and temporary
directories, a marker whose release failed) and
`removal_scales_with_directory_size` (a directory that used 300 names and a
1,000-level tree, removed under a 64-descriptor limit on a 256 KiB worker
stack, where recursion overflowed at a few hundred levels). The unit
tests `a_generation_follows_the_directory_an_observation_reads` (the directory a
root serves keeps its generation, another directory starts one, an observation
that cannot open the path keeps it, and reading the served directory again
admits that generation's leases),
`an_observation_a_release_overtook_installs_no_descriptor` (a release keeps the
generation and holds no descriptor, and the open it overtook serves that
operation alone) and
`lock_keys_stay_exclusive_across_concurrent_directory_removal` in the owner
module cover the slot transitions, the generation and the lock, removal and
temporary-file interleavings that the public API cannot produce reliably. The
App Server flows
`an_unloaded_session_releases_its_project_root`,
`a_closed_session_releases_its_project_root`,
`a_deleted_session_releases_its_project_root`,
`a_runtime_close_releases_the_project_root_a_load_registers`,
`an_agent_close_releases_its_sessions_project_roots`,
`one_of_two_sessions_in_a_project_keeps_its_root_in_use` and
`a_replaced_project_directory_is_served_after_a_session_is_created`
([idle_unload.rs](../../../../crates/dim-app-server/tests/transport_flow/idle_unload.rs))
check that a Session ending its use of a project releases the descriptor once no
Session uses it, that a Session the runtime closes without an App Server call
releases the root its load registered, that a replacement is served once the
path is registered again, and that the authorization keeps its index. The unit
test `a_closing_session_does_not_release_the_root_a_restore_owns`
([slots.rs](../../../../crates/dim-app-server/src/agents/tests/slots.rs)) holds a
restore inside its Session part and closes another Session on the same cwd, which
the loaded Sessions alone would read as the last use, and
`a_refused_restore_settles_the_ownership_of_its_cancelled_caller` ends the
ownership a restore registered when its caller stopped waiting. The unit tests
`settlements_end_only_the_ownership_they_name`,
`a_guard_dropped_after_a_later_load_keeps_that_loads_ownership`,
`a_runtime_close_ends_the_project_ownership_it_did_not_start` and
`a_delayed_close_watcher_keeps_the_ownership_of_the_load_after_it`
([tests.rs](../../../../crates/dim-app-server/src/agents/tests.rs)) check that a
settlement answers its project only when it removed the last owner, that an
Agent close ends every ownership it registered, that a guard dropped after a
later load of the same Session leaves that load's ownership, that a guard a
Session hands to its close settles an ownership this process did not close, and
that a first load's watcher delayed past the idle pass and a second load of the
same Session ID leaves the ownership the second load registered.

<!-- generated:plugin FileStorage — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | — |
| 提供 | `P:FileStorage`、`P:FsHost`、`P:dyn HostDispatch#fsHost` |
| 被注入于 | `HostPlatform` |
<!-- /generated -->

## Native streaming reads

`resolve(absolute)` identifies an existing authorized root without adding one;
paths use the root's canonical spelling. `open_read(path, cancel)` traverses held
directory handles and pins a regular file. Its `FileReader` reports the length
at open and reads at explicit offsets, at most 64 KiB per call. Concurrent reads
serialize access to the pinned descriptor; replacing a pathname does not change
an existing reader. The producer keeps published files unchanged in place.

Readers belong to the storage generation. Closing refuses new work, drains
accepted reads and closes retained descriptors. Caller cancellation is checked
before and after each bounded read. This native interface does not export binary
callbacks through the language bridge. UiResources uses it for the protected
resource routes in the [client UI contract](../../../design/client-ui-extensions.md).

## Path import

`import(path, source, limit, permissions, cancel)` copies an authorized absolute
source file into the authorized destination using `replace_from`. It opens and
pins the source, requires a regular file within the caller's limit, and rejects
length changes during copying. On Unix the open is nonblocking and rejects a
terminal symlink. The destination follows the existing lease, atomic replacement,
permissions and cancellation contract. The operation returns the copied byte count.
