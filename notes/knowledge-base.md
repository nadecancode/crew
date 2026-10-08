# Live workspace documentation

Design card: `knowledge-base-design`. Operator request:
`w-2d3dcc6b066fa029`. This is a proposed backend and wire contract, not an
implementation or a claim that the acceptance cases pass.

The human can read the workspace's Markdown while members edit it, compare
working copies, and inspect recorded versions. Reading documentation does
not deliver messages, wake agents, change cards, snapshot a jj working copy,
or add anything to an agent's prompt. The web experience is
[dashboard-features.md, Docs](dashboard-features.md#docs-knowledge-base).
Rendered Docs in the TUI and native apps are follow-ups; their data uses the
same portable records and authorization rules.

## Existing seams and constraints

Inspected source: `98a047c6`, with the reviewed D4 replay on the content
branch. The later dashboard feature notes change no backend behavior.

| Existing source | Consequence for Docs |
| --- | --- |
| `crates/crew/src/web/page.rs`, `Page::workspace` | An authenticated page's workspace connection becomes a pipe to its selected daemon. Use that connection; add no file-serving HTTP route. |
| `crates/crew/src/daemon/server.rs`, `Session`, `Conn` | Add a separate Docs watch forwarder and reply barrier. Do not replace `stream_task`, which already carries ordinary subscriptions or terminal output. |
| `crates/crew-protocol/src/types.rs`, `MAX_FRAME_BYTES`; `crew-client-transport/src/framing.rs` | A complete protocol frame is limited to 1 MiB. Image bytes cannot be embedded wholesale in a document reply. |
| `crates/crew/src/os.rs`, `open_no_symlinks` and `list_dir`; `git/beneath.rs`, `Root` | Reuse descriptor-relative, no-symlink traversal on Linux and macOS. `Root::read_text` currently caps administrative files at 64 KiB and loses error detail; it is not a document reader. |
| `crates/crew/src/vcs/mod.rs`, `Vcs`; `vcs/jj.rs`, `diff` and `status` | Existing diff/status operations can snapshot a jj workspace. They are unsuitable for a read-only Docs observer. Add a separate bounded history read path. |
| `crates/crew/src/git/mod.rs`, `output` | The launcher funnel is reusable, but this runner drains output without a byte cap and permits a 300-second production timeout. Docs needs smaller bounds before allocation. |
| `crates/crew/src/native/policy.rs` | Gateway policy is default-deny. New Docs reads need an explicit Read-tier decision and tests; index, action and terminal lanes do not acquire document access implicitly. |
| `web/src/text.tsx`; `web/src/views/Inspector.tsx`, `Diff` | Existing text is escaped and only HTTP(S) links are active. The current diff renderer styles plain unified-diff lines; it does not provide a Markdown parser or a structured hunk API. |
| `crates/crew/src/web/http.rs`, CSP | Keep `img-src 'self' data:` and the existing script/style restrictions. No remote image fetch, inline HTML, injected style or later HTTP asset request. |

All record and method names below are proposals for the coordinated
`feature-protocol-plan`, owned by native-architect. That plan decides the
final capability shape, method inventory and compatibility treatment across
Docs, epics, usage and asides. Docs can remain additive to v6 if its new
pushes reach only clients that explicitly opt in and its capability is
absent on old servers. Do not independently increment the protocol version
or make older clients parse a new unsolicited push variant. The FFI ABI and
generated bindings need their own compatibility checks.

## Sources, selection and configuration

### Two kinds of source

The service reads only source roots registered by the daemon:

- **Workspace (working copy):** the operator's current checkout, anchored
  at `Daemon.workspace`. It can be uncommitted or on a branch other than
  main. The header shows its known revision and dirty/unknown state; it is
  never described as necessarily landed.
- **Member worktree:** a worktree created or adopted through the daemon's
  existing workspace lifecycle. Its source names the member and known
  `agents/<id>` branch. A stopped member's registered worktree can remain
  readable. Removal invalidates its source handles and closes its watches.

Source paths never come from a browser, Markdown link, task artifact or
member-supplied display string. Validate a member's stored workdir against
the daemon's workspace registration and the existing protected-path rules
before opening it. Do not scan arbitrary folders found below `.crew`.

Shared-workspace members and an orchestrator using the operator checkout
share one source rather than duplicating every file. The index can list the
members associated with that source. A source owner is not an observed
author: say “designer's worktree”. Show “by <author>” only when recorded VCS
metadata supplies an author. Filesystem timestamps are observation hints,
not authorship evidence.

The normalized relative path identifies one logical document across
sources. The tree has one row with source variants, rather than one copy of
the tree per member. Each variant states `same_markdown`, `different`,
`only_here` or `unknown` relative to the workspace variant. Compare actual
Markdown byte hashes, never mtimes. Image-only changes invalidate an open
rendered document separately; `same_markdown` does not claim equal images.

### Defaults

Index UTF-8 regular files whose extension is `.md` or `.markdown`:

- in the workspace/source root and its immediate child directories;
- recursively under `notes`, `review` and `docs`, to the scan depth limit;
- recursively under explicitly configured extra scan roots.

Untracked Markdown is included so writing a new document is visible before
its first commit. There is no `git add`, jj snapshot or requirement to commit
before reading. The index distinguishes a working copy from recorded
history. A nonexistent scan root in a member's older checkout is simply
absent.

Git ignore rules are not the Docs selection policy: this repository itself
ignores new files under `notes/` while members are authoring them. An ignored
Markdown file in an allowed scan root remains visible. The explicit Docs
exclusions and safe-read rules are what constrain the observer.

Hard exclusions apply at every content-path component **inside** a registered
source root, also to linked assets. They do not reject the registered root's
absolute ancestor chain: a valid member root normally lives under
`.crew/worktrees/<id>`. Scanning the workspace's `.crew` is still forbidden;
admitting that separately registered member root is a distinct operation.
Excluded components are:
`.crew`, `.git`, `.jj`, hidden files/directories, `node_modules`, `target`,
`dist`, `build`, `out`, `coverage`, `vendor`, `secrets` and `credentials`.
Comparison of these exclusion names is ASCII case-insensitive. Runtime
configuration and private homes therefore do not become docs. Symlinks,
submodule directories, sockets, FIFOs, devices and non-UTF-8/control-bearing
filenames are not indexed. A regular content file with multiple hard links
is refused too. Configured extra roots cannot override these exclusions.

A linked image need not itself be in a Markdown scan root. It must be a
referenced, allowed image under the **same registered source root**, pass the
same hard exclusions and safe-open checks, and fit the image limits. No
unreferenced asset can be requested through a general file-read endpoint.

### Operator configuration

Use a `[docs]` section in the existing global, user and workspace crew
configuration scopes, with the same precedence order. This is human
configuration, not a field in a member's toolbox or standing instructions.

```toml
[docs]
enabled = true
extra_roots = ["design", "crates/crew/docs"]
exclude = ["notes/private", "review/private.md"]
```

`enabled` defaults to true. Both lists default empty and replace the
corresponding lower-scope list when present. Entries are literal relative
paths, not globs or shell expressions; reject absolute paths, `..`, NUL,
backslashes and empty entries. `extra_roots` names directories or individual
Markdown files. `exclude` removes a file or a complete directory subtree.
The built-in root scan and recursive defaults remain unless excluded.
At most 32 entries are allowed in either list, each at most 1,024 UTF-8 bytes.

Load only the typed Docs section. Never send the complete configuration file
to a client: it can contain endpoint or toolbox credentials. Coordinate its
recognition with `endpoints.rs::FileConfig`, whose strict parser currently
needs an explicit field for a new top-level section. Configuration changes
rebuild the Docs policy/index and invalidate affected handles; they do not
reload or restart the team. A malformed update keeps the last valid policy
and reports a metadata-only configuration error. Disabling Docs closes
watches, drops retained bodies/assets and clears the index.

Older binaries with that strict parser can reject a `[docs]` table. The
release notes must distinguish configuration compatibility from additive
wire compatibility; use the new setting only with binaries that recognize it.

Selection rules prevent crew from publishing its private files. They are
not a secret detector for prose or screenshots deliberately placed in an
allowed documentation path. Such content is readable by the workspace's
authorized readers. Operators can exclude a path or disable Docs; the
service never claims to redact arbitrary credentials from document content.

## Safe reads and resource limits

Open each registered source root once, without a symlink in any component,
and hold its descriptor. Resolve content paths beneath that descriptor with
`open_no_symlinks(..., beneath=true)` and nonblocking read flags. Require a
regular file and inspect its descriptor metadata before reading. Validate
normalized relative components before the syscall, using the stricter
no-`..` rule on every platform. A prior `canonicalize`/prefix check followed
by an ordinary path read is not sufficient.

Directory enumeration uses the descriptor, not a path reopened from a watch
event. Watch events are hints to rescan an approved source; their paths are
never trusted read arguments. Check membership and source generation again
before publishing a result. Replacing or removing a source cancels its jobs
and invalidates its leases. A platform without the required safe opener
reports Docs unavailable rather than falling back to unrestricted reads.

Read through the opened descriptor up to the limit plus one byte. Compare
file identity, size and change metadata before and after the read. Retry an
unstable file at most twice, then keep the last good display with an
`unstable_file` notice. Hash the bytes actually parsed. Do not publish a
partially written document as a successful immutable version.

Initial hard limits, with explicit truncation/unavailability reporting:

| Resource | Limit |
| --- | ---: |
| Registered sources per workspace | 64 |
| Directory depth below a source | 16 |
| Enumerated entries per source per scan | 20,000 |
| Logical documents / source variants per workspace | 4,000 / 16,000 |
| Markdown bytes per version | 256 KiB |
| AST nodes / nesting depth | 4,096 / 32 |
| Headings per indexed document | 128 |
| Local image bytes / aggregate per rendered document | 2 MiB / 8 MiB |
| Image dimensions / decoded pixels | 8,192 per axis / 16 million pixels |
| Image references per document | 64 |
| Complete serialized Docs reply target | 512 KiB, and always below the transport's 1 MiB cap |
| Raw asset chunk request maximum | 192 KiB; reduce it to fit the actual serialized frame |
| Index page entries / history page entries | 200 / 50 |
| Selected live documents per connection | 2 |
| Retained seen versions per document | 16 or 4 MiB of Markdown, whichever is reached first |
| Total retained document/AST/image/history-cache bytes per workspace | 128 MiB |
| Concurrent read/parse jobs / history jobs per workspace | 2 / 1 |
| History subprocess deadline / stdout / stderr | 5 s / 2 MiB / 16 KiB |
| Unified diff body / input lines per side | 64 KiB / 8,192 |

Enforce allocation and expansion budgets during parsing and history reads,
not only after building an oversized value. Check the **final serialized
frame**, including JSON escaping, base64 and any transport envelope, before
queueing it. Raw-byte estimates are not a frame-size check. A document that
cannot fit the closed representation returns metadata and `too_large`; it
does not disconnect the socket or silently lose paragraphs. Index limits
include `limited=true` and omitted counts when known.

Opaque ids/cursors are at most 256 bytes; relative paths and branch labels
at most 1,024 bytes; display titles and indexed heading labels at most 512
bytes. A page stops at its serialized byte budget even before its entry
count limit. Truncating a display label does not change the underlying id
or anchor, and a shortened outline is marked limited. For a large index
entry, omit further heading labels first and mark `headings_limited`, while
retaining its path and source-variant identities. An entry that still cannot
fit becomes a bounded unavailable summary, not an endless empty page.

Large assets are pulled in chunks over the existing connection. Limit each
connection to two document reads and two asset requests in flight; clients
load visible images a few at a time. Retain at most one pending Docs metadata
push per connection, coalescing newer revisions behind it. Never enqueue
all images into the ordinary 1,024-frame connection queue. A reader that
cannot keep up receives a resync marker, not an unbounded backlog.

The byte-bounded cache can evict old seen versions and assets. A live read
either obtains a bounded snapshot lease or returns `busy`; it never pins
unbounded data to satisfy more viewers. Eviction is visible as
`snapshot_expired` or an expired-image placeholder. Do not substitute a
current file for an expired historical asset.

## Parsing and images

Parse Markdown on the daemon, off the async executor and outside the model,
task and VCS mutation locks. The implementation selects a maintained
CommonMark parser with table and task-list extensions, pins it in Cargo.lock,
and verifies the default CLI's Rust 1.82 gate. The parser's HTML renderer is
not part of the design. Browser and native clients receive only a closed
syntax tree; no client-side Markdown or sanitizer dependency is required.

Proposed node variants:

```text
Block = Heading(level 1..6, anchor, inline[])
      | Paragraph(inline[]) | CodeBlock(language_hint?, literal)
      | Quote(block[]) | List(ordered, start?, item[])
      | Table(alignments[], header[cell], rows[cell[]]) | Rule
ListItem = { checked: bool?, blocks: Block[] }
Inline = Text(literal) | Emphasis(inline[]) | Strong(inline[])
       | Code(literal) | SoftBreak | HardBreak
       | Link(label[], target: LinkTarget) | Image(alt, asset: ImageRef)
```

`ImageRef` is a checked index into that snapshot's image table; it cannot
name an arbitrary asset. Repeated references can share one image's bytes,
but all references count toward the reference limit. The aggregate image
byte budget counts distinct admitted assets in first-reference order.

Raw HTML is literal text/code, not an HTML node. Unsupported syntax stays
literal. Code fences are never executed: no Mermaid, embedded script,
math engine, syntax-highlighting service or plugin invocation. Clients map
known node variants to their own fixed components and CSS classes, never
to arbitrary tag/attribute names. Task-list checkboxes are read-only.
Terminal clients strip terminal control sequences when rendering text.

Each top-level block carries a content-derived `block_key` and source line
range. Duplicate blocks have deterministic occurrence suffixes. Headings
have deterministic anchors and an outline entry. Clients keep the top
visible heading while updating, falling back to its surviving ancestor or
nearest source line if it disappears; they never steal keyboard focus.
“Show changes since opened” compares the current block keys with the page's
retained opening snapshot, without asking an agent or modifying either
snapshot. It is distinct from a recorded line diff.

Links are resolved by the server to a closed `LinkTarget`:

- `Doc { doc_id, preferred_source?, anchor? }`, only for known, policy-allowed
  paths; relative links resolve from the document's directory in its source;
- `Anchor { anchor }`, only for a heading in that version;
- `External { href }`, only an HTTP(S) URL, rendered with
  `rel="noopener noreferrer"` in a new tab;
- `Text { reason }`, for other schemes, malformed links or unknown paths.

Decode an encoded path once, normalize its components within the source,
then use the registered index lookup. Reject an escape, rather than passing
`..` to the opener. RPC methods never accept an arbitrary filesystem path.
`file:`, user-supplied `data:`, `javascript:`, protocol-relative URLs and
backslash variants do not become active links. No URL is fetched by the
daemon.

An image node contains its relative display path, content hash, media type,
size and opaque asset id, or a reasoned placeholder. V1 supports static PNG,
JPEG, GIF and WebP. Validate the format, dimensions and limits
with a bounded metadata reader; do not trust the extension alone or decode
an unbounded raster to discover its size. SVG and animated raster formats
are unsupported in v1. An image whose safe size/structure cannot be
established remains a placeholder. Embedded image bytes are retained as immutable
assets when admitted to the cache. The client assembles a `data:` URL only
after fetching and checking the complete asset's hash and size. It caches
by hash and preserves alt text during loading. Remote images are not fetched;
show their alt text and a safe HTTP(S) link when available.

SVG is never inserted as HTML, a document, an image source, an object/embed
or an interactive WebView. No XML entity expansion, external resource
resolution or SVG thumbnail renderer is permitted. The web acceptance
fixture must prove no script, external resource or extra HTTP request occurs.
Use alt text first, followed by a textual reason, such as “Image not shown ·
SVG is not supported”, “Image not shown · animated images are not supported”,
or “Image too large to show (9000 × 400 px)”. A later native renderer follows
the same supported-format contract. Image-only
changes produce a document invalidation even if the Markdown hash is the
same. Recorded images come from the same commit tree as the Markdown; seen
versions use their captured asset bytes, never the current path by accident.

## Watching, versions and history

### Watch lifecycle

Start a workspace's bounded index on the first authorized Docs read. Start
live filesystem watches only while a connection subscribes to Docs. Close
them after the last subscriber leaves. Watch validated directories and the
parent of an open file so an editor's atomic replacement is detected. The
workspace/member registry and configuration can add or remove sources.

A page may request an index-only watch with no selected document, to keep
known-path links current in Tasks and the bus. This sends metadata only and
does not create seen versions or render/fetch images. Scanning is incremental:
yield after 256 directory entries, 4 MiB of reads or 25 ms of CPU work,
whichever comes first. Walk in deterministic path order and report the
building/limited state rather than blocking a page for a full large tree.

Use native filesystem notifications with a bounded dirty set, plus a
15-second reconciliation while watched to recover missed events. If native
watching is unavailable, a five-second backend reconciliation mode is
allowed and is reported to the UI as delayed updates. The browser never
polls. A watch overflow discards per-path event guesses, marks the index
stale, and schedules a bounded rescan. Rescans do not hold the task planner
or delivery locks.

Coalesce saves to at most one published update per source/document per
second, using monotonic time and a trailing update. Continuous edits must
still publish the newest stable bytes at each eligible boundary. A burst
does not reset a debounce forever. Image dependencies of open documents
participate in the same invalidation path.

Register a watch before capturing its initial index page. Queue that reply
before forwarding later changes, as the existing summary/snapshot barriers
do. Every push names a Docs epoch, watch id and increasing index revision.
A gap, overflow or expired page cursor requires a new index capture. A slow
old read cannot replace a newer selection or document version in the client.

File deletion leaves the last displayed content in the page, visibly marked
removed. The service keeps only bounded tombstone/lease metadata necessary
for that already-open document. Workspace revocation, grant expiry or source
policy removal instead invalidates the affected data and handles; the client
must not retain a usable preview under a lost authorization scope.

### Recorded versus seen here

**Recorded** history follows commits reachable from the registered workspace
head and current known member branch tips. Capture the tip ids for a history
page sequence; a branch advance does not change the meaning of an old page
cursor. Deduplicate a commit visible through several source branches while
keeping its source labels. Return author, recorded time, subject, commit id
and a jj change id only when known from trustworthy VCS provenance. Do not
invent a change id from a Git SHA.

For crew's Git-backed jj store, the first implementation can read the
published Git commit graph and branch tips, including commits exported by
crew. Label this **published recorded history**. Unsnapshotted/unpublished
working-copy edits belong to Seen here, not to a synthetic commit. Do not
run ordinary `jj status`/`jj diff`, `git import`, or any workspace-snapshotting
operation to manufacture a history entry. Native jj-only or unavailable
object stores return a reasoned history capability instead of changing the
repository. Renames are delete/add in v1; history does not guess a path
outside the selected document's permitted name.

Add a dedicated read-only VCS interface for bounded file history and blob
reads. Client input selects only a server-issued history version; it cannot
supply an arbitrary revset, command option, object id or pathspec. The reader
verifies ancestry against the captured tip and resolves the exact file in
that commit tree, requiring a regular blob rather than a symlink or submodule.
Policy exclusions also apply to historical paths and their linked assets.

Git plumbing, if used, runs against the daemon's known project store, with
literal paths and fixed argument arrays, without credentials, hooks, filters,
textconv, external diff, pager, signature programs, replacement refs or lazy
network fetch. Reject external object alternates and untrusted configuration
indirection rather than reading another repository. The registered VCS
administrative/object store is the one necessary metadata authority; it is
not added to the document scan roots. Do not reopen a worktree's attacker-
replaceable `.git` file to discover it. Reuse the safe root/launcher seams,
but add a bounded reader and a read-only launch profile where required;
the existing writable helper profile is not sufficient unchanged. The
implementation must prove the same no-helper/no-network/no-escape behavior
on each supported history backend before advertising that capability.

Do not validate a writable Git config and then let an ordinary command
reopen it. Use a controlled read view with fixed configuration and confined
object access, or a descriptor-backed object reader; the hostile-config
race belongs in K15. If the platform cannot provide that boundary, report
recorded history unavailable while still serving safely opened working
copies. This is a visible capability limit, not permission for an unsafe
host fallback.

**Seen here** consists of versions actually returned to this connection's
document view, including uncommitted saves. It is a bounded in-memory
history, not a database of all filesystem activity. The UI lists only the
versions it received, labelled by source and observation time. Retain up to
16 versions and 4 MiB of Markdown per document, subject to the global cache
budget; bodies and images are never written to the event log or roster.
Disconnect/restart clears this history. The timeline footer says “Seen here:
up to 16 versions or 4 MiB of this file while connected; older versions may
expire.” Remove unselected expired entries; a selected expired version shows
a neutral unavailable state rather than silently switching to current
content. Old assets can expire separately and must show an expired-image
placeholder.

For a selected expired entry, keep its selection and header path. The body
says “This version is no longer kept here. Seen-here versions expire when
the connection drops or space runs low.” Offer View Workspace and, when a
recorded commit exists, View recorded <short sha>. Never switch implicitly.

Recorded and seen versions produce the same immutable document DTO. Diff
two authorized versions of the same logical path using a bounded line diff
in Rust, without external diff programs. `Compare with Workspace` captures
the current workspace working copy and names both captured versions. Return
plain unified-diff text and counts, with `truncated=true` when the output cap
is reached. Reuse the existing web diff line styles; extracting that
presentation into a shared component is UI work. A diff beyond the line or
time budget returns a reason, not an unbounded computation.

## Proposed portable records and calls

Place configuration-free records in `crates/crew-protocol/src/docs.rs`.
Opaque ids are bounded strings, not file paths or bearer substitutes. They
are useful only on an authorized connection in the indicated workspace,
Docs epoch and source generation. Source roots and absolute host paths are
private server state.

```text
DocSource = {
  id, generation, kind: workspace | member | shared,
  member_ids[], display_name, branch?, head_commit?,
  state: ready | missing | removed | unavailable, reason?
}
DocRef = { doc_id, source_id, source_generation }
DocVariant = {
  source_id, source_generation, markdown_hash?, bytes?,
  observed_at_ms?, title?, headings[], headings_limited,
  state, reason?, comparison
}
DocHeading = { anchor, title, level, start_line }
DocEntry = { id, relative_path, variants[] }
DocIndexPage = {
  epoch, revision, sources[], entries[], next_cursor?,
  phase: building | ready | disabled | unavailable,
  watch_mode: events | reconcile_only | none, limited, reason?
}
DocVersion = {
  id, document: DocRef, kind: working | recorded | seen,
  markdown_hash, bytes, observed_at_ms?, recorded_at_ms?,
  commit_id?, change_id?, author?, subject?
}
DocSnapshot = {
  epoch, snapshot_id, version: DocVersion, markdown, blocks[], outline[], images[],
  render_hash, notices[]
}
DocImage = {
  asset_id?, relative_path?, hash?, media_type?, bytes?, alt,
  state: ready | remote | too_large | blocked | unsupported | expired,
  reason?, safe_external_href?
}
DocAssetChunk = { snapshot_id, asset_id, hash, media_type, offset, total_bytes,
                  data_base64, next_offset? }
DocHistoryPage = { epoch, doc_id, captured_heads[], recorded_versions[], next_cursor? }
DocDiff = { left: DocVersion, right: DocVersion, unified_diff,
            added_lines, removed_lines, truncated }
```

`DocSnapshot` body availability is a result variant: either the complete
bounded snapshot or `{ state: unavailable, document, code, detail, bytes? }`.
Missing and unavailable content must not be encoded as an empty successful
document. `markdown` is source data for copying/diffs, never an HTML sink.
All numeric byte counts use raw bytes and state their units.

Image media types form a closed static-raster enum. Clients validate node
counts, nesting, ids, media types and byte bounds while decoding; being
authenticated does not make an unbounded or malformed response acceptable.

Document/variant states are `present`, `missing`, `removed` or `unavailable`;
comparison is `same_markdown`, `different`, `only_here` or `unknown`.
Content failure codes include `disabled`, `forbidden_path`, `source_gone`,
`too_large`, `invalid_utf8`, `unsupported_image`, `unstable_file`,
`history_unavailable`, `object_missing`, `snapshot_expired`, `stale_cursor`
and `busy`. Authentication/authorization failures use the existing RPC
errors. Details contain only public relative names, never a host path,
credential value or unfiltered subprocess stderr.

Proposed read family (final names/capabilities go through the protocol plan):

| Call | Input and response |
| --- | --- |
| `docs_index` | Optional frozen page cursor and limit; returns `DocIndexPage`. No subscription side effect. |
| `docs_watch` | Replaces this connection's selected live `DocRef[]` (at most two). Registers the index watch first and returns `{ watch_id, initial_index_page }` before any matching push. |
| `docs_unwatch` | The connection's watch id; ends that watch, idempotently. |
| `docs_read` | `DocRef` plus `current` or a version id issued by history/a prior read on this connection; returns a bounded immutable snapshot/result. |
| `docs_asset` | Snapshot id, asset id, offset and bounded maximum bytes; returns one chunk. Never accepts a path/URL. |
| `docs_history` | Known logical doc id, optional registered source ids and frozen page cursor; returns published recorded history. Seen-here rows remain connection/view-owned. |
| `docs_diff` | Two authorized version ids for the same logical doc; returns `DocDiff` or a bounded failure. |

Doc ids are assigned from the approved index. Relative paths in task/message
text resolve through that known-path map; a path-like string alone never
authorizes a read. The map also supplies the first available source when a
document exists only in a member worktree. A `§Heading` suffix matches the
first outline title with that prefix in document order; ambiguity is not
resolved by inventing an anchor. “Referenced by” remains a client query of
the tasks/messages/events loaded in that page and says “in loaded history”.

Titles/headings belong to a source variant, never to a fictional merge of
all members' documents. A document's own relative links prefer its current
source, then the workspace, then a deterministic available member source.
Typed file artifacts can prefer their recorded agent's registered source.
For a generic path mention, prefer the workspace when present. Heading
resolution uses the version actually opened, not another variant's outline;
missing/truncated matches open the document without a fabricated anchor.
Do not match a doc path inside an already-recognized external URL.

One opt-in push family carries **metadata**, not full documents or images:

```text
DocsChanged = {
  epoch, watch_id, from_revision, to_revision,
  upserts[], removed_doc_ids[], changed_selected[],
  history_sources_changed[], resync_required
}
```

`changed_selected` identifies content, image, removal, policy or availability
changes. Clients request the newest snapshot through the same connection.
Large deltas become a small resync marker. Docs pushes are never broadcast
through the ordinary agent-facing subscription, message bus or runtime
delivery channel. No live Docs list/count/body enters a standing prompt or
MCP tool description. V1 adds no agent MCP tool for this human observer.

### Authorization and shared clients

- Local calls require the operator role. An agent identity, including the
  orchestrator's, cannot use Docs to read another member's worktree through
  an observer endpoint.
- Web calls use the existing paired, admitted workspace connection and are
  read-only. No path or version from another joined workspace is accepted.
- Gateway calls require Read on that workspace. Watch/unwatch are allowed
  on the workspace lane only; non-watching reads can use the workspace or
  read lane. Index, action and terminal lanes cannot use them. Revocation,
  expiry and disconnect cancel jobs/watches before further publication.
- Leases and cursors are bound to the authenticated connection and scope.
  A native reader must keep its read-lane connection for the complete
  read/chunk operation. Handles cannot be reused on a new connection or
  attempt; reconnect explicitly recaptures the index/history/snapshot.

Extend the actual resource-read seams (`ResourceKind`, `ReadOptions`,
`ResourceValue` and resource requests/keys) in `crew-client`, rather than
creating another client state engine. Add connection-scoped Docs watch
effects only with the transport owner. Bind every result to `ClientScope`,
operation/attempt and selected version; discard obsolete responses. The
gateway policy method inventory, native transport framing, protocol method
encoding, FFI converters and web decoders must be updated together under
`feature-protocol-plan`. Native Docs controls stay unavailable until their
driver/renderer is implemented; adding DTOs alone must not advertise a
working screen.

## Acceptance cases

Use a fixture workspace plus two independent member worktrees, a shared-
workspace member, both Git and crew's Git-backed jj mode, and authenticated
local/web/gateway connections. No model or provider request is needed.

| ID | Pass condition |
| --- | --- |
| K01 Sources | Root/immediate-child Markdown and recursive defaults appear; an untracked member document appears; separately registered member roots under `.crew/worktrees` work without scanning the workspace's `.crew`; shared roots deduplicate; stopped/removed sources follow the specified lifecycle. |
| K02 Configuration | Layer precedence and whole-list replacement hold; extra roots/exclusions work on every source; invalid roots keep the last policy; disabling clears bodies, handles and watches. |
| K03 Exclusions | Private crew/runtime/git files, hidden paths, build/vendor trees, excluded docs and unreferenced assets cannot be read by guessed ids or link syntax. Exclusions apply relative to each root and cannot be bypassed by a nested `.crew` inside a member root. |
| K04 Safe filesystem | Symlink swaps in any component, hard links, FIFOs, sockets, devices, traversal, encoded traversal and root replacement never read an outside file or hang. Prove Linux and macOS opener behavior separately. |
| K05 File stability | Atomic replacement and a file changing during read produce a consistent version or explicit unstable state. Oversized/non-UTF-8 files are not silently truncated into successful documents. |
| K06 Render safety | Every supported node renders; raw HTML, event handlers, scripts, malicious URL schemes and unknown node variants remain inert. No dynamic tag/style sink or code-fence execution. |
| K07 Images | Allowed static raster images load through bounded chunks; hashes/offsets are checked; byte/pixel/dimension limits are visible; remote images make no request; SVG/animation stay placeholders; historical images never use current bytes. |
| K08 Transport limits | Worst-case JSON escaping and base64 at all boundaries stay under the final 1 MiB frame cap. Oversized ASTs and diffs give bounded errors/truncation, not disconnects or excessive allocation. |
| K09 Watch barrier | An edit between registration, capture and first reply is in the reply or a later revision, never lost. Old reads cannot overwrite a new selection/version. |
| K10 Save bursts | Twenty saves and continuous saves produce at most one update per file per second, including the trailing stable state. An image-only save invalidates an open preview. |
| K11 Watch recovery | Rename/delete/recreate, source removal, watcher overflow and missed events converge after bounded reconciliation. No browser polling or runtime wake is introduced. |
| K12 Authorization | Agent and unauthenticated reads fail; cross-workspace ids fail; gateway tier/lane checks hold; revocation/expiry/disconnect stops data publication and invalidates client state. |
| K13 Resource pressure | Slow clients, many sources, excessive files, parser expansion, cache pressure and concurrent history requests stay within every declared cap and expose resync/expiry/busy reasons. |
| K14 Observer isolation | No bus message, inbox item, task/agent mutation, prompt change, jj snapshot, repository mutation or credential-bearing event occurs during reads/watch/history/diff. |
| K15 Recorded history | Git and published jj histories return the correct per-file versions across workspace/member heads; cursors pin heads; symlink/submodule blobs and external alternates are refused; no helper/filter/network program runs. |
| K16 Seen here | Only received versions appear; retention/eviction and restart loss are visible; raw bodies never enter the database, event log or roster; expired images never masquerade as current ones. |
| K17 Comparisons | Working-copy and recorded/seen comparisons name both immutable captures; textual diff counts/hunks match fixtures; large comparisons stay bounded; Workspace is not labelled landed. |
| K18 Navigation | Known-path and heading links resolve through the index, unknown paths stay text, the member-only fallback is correct, source labels do not invent authors, and referenced-by scope is explicit. |
| K19 Client parity | Portable DTO round-trips, typed failures and stale-scope rejection agree across web/shared core/FFI. Native UI capabilities remain truthful while Docs UI is deferred. |
| K20 Presentation | Designer's F07–F11 and F18–F19 pass: reading position/focus, deleted-file notice, safe rendering, history, chunks, network/CSP and existing frontend budgets. |

## Implementation ownership

Assign cards only after this design and the coordinated protocol plan are
approved. The following boundaries avoid parallel edits to central files.

1. **Config and portable contract.** New `crew-protocol/src/docs.rs`, typed
   Docs settings, configuration recognition in `endpoints.rs`, and the
   reviewed method/capability additions in `crew-core/src/rpc.rs`. Coordinate
   central RPC/FFI/client files with native-architect. Covers K02/K08/K19.
2. **Safe source index and observer.** New `crew/src/docs/{mod,source,index,
   watch,cache}.rs`; narrow `os.rs`/safe-reader reuse; daemon lifecycle and
   registry hooks. No VCS mutation helpers. Covers K01–K05/K09–K11/K13/K14.
3. **Parser, assets and version comparison.** New
   `crew/src/docs/{markdown,assets,diff}.rs`; bounded closed AST and snapshot
   leases. Parser dependency/lockfile and Rust 1.82 verification belong here.
   Covers K05–K08/K13/K16/K17.
4. **Published history reader.** A separate read-only VCS interface and
   bounded runner, with explicit Git/jj store adapters; read-only sandbox
   profile where needed. Preserve existing commit/diff/status behavior.
   Covers K04/K13–K15/K17.
5. **Authenticated delivery and shared client.** New daemon Docs handlers;
   `daemon/server.rs` connection/watch state; gateway policy and lane tests;
   resource-read records/commands/state, native driver and FFI conversions
   coordinated with the protocol owner. Covers K08–K14/K19. WebSocket
   transport remains the existing workspace pipe.
6. **Web Docs and QA.** The designer's Docs slice owns the tree, fixed AST
   renderer, asset assembly, history/diff presentation, known-path links,
   route/key wiring, accessibility and budget measurement. Backend owners
   provide the hostile filesystem/Markdown/transport fixtures and K01–K19
   evidence; QA owns F07–F11/F18–F19 and K20. No native Docs screen is claimed
   by this web slice.

Remaining implementation choices are the pinned Markdown parser and native
watch library, selected and measured within those cards. Optional full-text
search, editing/exporting docs, arbitrary repository browsing, rename-following
history, native rendered Docs and automatic model consumption of documents
are outside v1. None is approximated with an unsafe file endpoint or a live
agent message.
