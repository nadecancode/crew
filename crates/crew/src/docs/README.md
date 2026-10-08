# Documentation observer service

`Service` implements the negotiated `FeatureService` and `FeatureWatch` ports.
It is not automatically registered or advertised by this module. The daemon
checks authentication, negotiated version and lane/tier; the service also
requires `Caller::Operator` and an active run and physical connection epoch.
There is no agent tool, prompt fragment, runtime command or delivery dependency.

The host supplies `Registration` records from its validated workspace/worktree
registry and a typed `Policy`. These are trusted Rust inputs, not RPC inputs.
Never construct a registration from a client path, Markdown destination or
member display string. Shared directory identities are deduplicated; a stopped
member can remain registered, while removal invalidates its handles. Refresh
published tips without changing source identity so captured history cursors keep
their original meaning. `configure` parses only `[docs]` and keeps the last good
policy on error. Older strict configuration parsers may reject this table.

Indexing begins on an authorized request and yields between bounded steps.
Native notifications discard their event paths; they only request safe rescans.
A subscriber activates native watches and a trailing, one-second publication
boundary. Reconciliation runs every 15 seconds, or every five seconds when
native watching fails or exceeds its directory budget. Each stream keeps one
pending metadata update. P2 queues the watch reply before forwarding that stream.
Connection cancellation is sticky; blocking jobs retain their permits until
finished and recheck scope before publishing.

Every filesystem component is opened without symlinks through `os.rs`.
Content paths cannot contain `..`, even on macOS; links decode once and normalize
before their known-index lookup. Nested repositories, hard links, private/build
components, devices, pipes and sockets are not content. The policy is not a
secret detector for prose deliberately placed in an admitted docs folder.
Markdown becomes the closed protocol AST; raw HTML remains literal. Images use
bounded static-container metadata inspection, not a pixel decoder. Clients must
still handle ordinary raster decoding failures without fetching replacements.
Only assets referenced by a captured snapshot can be read, in bounded chunks.

Captures and cursors belong to one connection and Docs epoch. Captures include
immutable asset bytes. No recovery reads a current file for an expired version.
The service bounds its cache, index, cursors and connection registries within the
128 MiB retained-data budget. Additional admission bounds are 32 connections,
256 captures and 128 issued recorded-version descriptors per connection, four
five-minute cursors per connection, 4 MiB of scan path bookkeeping, and at most
64 native watched directories per source / 4096 total. Reconciliation remains
available when native watch coverage cannot be complete. A single JSON count
writer bounds escaped serialization before a response Value is allocated; P2
also bounds the final frame including its envelope.

Recorded history is optional even when working-copy reads are available.
`history::Reader::from_store` accepts the daemon's known `vcs::Store`, never a
worktree's `.git` file. Its private administrative view contains fixed config
and empty refs. Git sees only that view, a read-only object directory and public
executable/library mounts. Source configuration is never mounted; the object
`info` directory is hidden to prevent alternate-store races. No worktree, home,
daemon state, credential, writable repository mount or network is provided.
Subprocesses have a five-second deadline, 2 MiB stdout and 16 KiB stderr bounds.
Failures never trigger a host fallback. Current history support requires Linux,
crew's probed bubblewrap/Landlock boundary and SHA-1 Git objects. Unsupported
platforms or stores return `history_unavailable`; working Docs still use their
safe platform opener. Git-backed jj uses its published Git graph only; no jj
snapshot/import command is used by this service.

The service tests cover hostile source/link/AST inputs, scope and cancellation,
immutable chunks and retention, frozen cursors, line diffs and watch queues.
`CREW_TEST_SANDBOX=require cargo test -p crew --test docs_history` executes the
confined reader rather than counting an unavailable sandbox as evidence.
Actual macOS opener execution, gateway framing/revocation integration, client
lease invalidation and frontend rendering/network/CSP checks remain explicit
platform/transport/UI gates. A Linux test is not evidence of those executions.

The capture budget uses a global least-recently-used eviction order across
operator connections. One reader can therefore expire another reader's oldest
captures. An active watch receives expiries on that same physical connection;
a read-lane capture reports `snapshot_expired` when it is next accessed. An
unavailable `objects/info` masking mountpoint makes recorded history unavailable
before launch; the observer does not create repository directories to repair it.

History pages batch log records per captured head, check blob types/modes with
literal tree lookups, then batch object sizes and content through
[`git cat-file`](https://git-scm.com/docs/git-cat-file). Each content batch stays
inside the 2 MiB subprocess output bound. A page-level
[`git rev-list`](https://git-scm.com/docs/git-rev-list) exclusion walk computes
which page commits are reachable from each captured head; it does not launch a
merge-base process for every row/head pair. Frozen cursors retain bounded
lookahead records and native offsets instead of restarting a log walk per row.

A Docs `resync_required` update, epoch change or revision gap invalidates all
cached Docs values and pending reads in the client, including retained leases
and assets. Selected document references stay selected; the renderer discards
its retained bodies and recaptures before displaying them. This is also the
bounded fallback when more than 200 expiry ids would be needed. The physical
connection remains open, but an expired opaque version still fails on the server
even if its individual expiry id was omitted. History-source ids are merged
independently when pending metadata is coalesced.
