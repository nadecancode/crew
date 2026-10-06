# Apple implementation status

The approved navigation foundation is `4479a301`. Substantive implementation
uses the reviewed N2A source at `a159bd03` and the connection experience at
`5015460a`. Source now composes the real local facade on macOS and the
released shared availability query for action controls (see "Shared action
availability"). Gateway composition is still required for iOS; source
preparation and unperformed native execution remain distinct. Later sections
supersede the remaining-gate lists of earlier checkpoints.

## Host storage and presentation checkpoint

`CrewHost/AppleDeviceKeyVault.swift` adds a host-only Ed25519 identity helper.
It creates a private key using CryptoKit's OS-backed entropy, stores its 32-byte
seed as a device-only, nonsynchronizing, unlocked Keychain item, and returns
only its public key. Mac storage selects the data-protection keychain rather
than falling back to a legacy or plaintext store. Missing, duplicate, corrupt,
locked and unavailable storage are explicit errors. Retiring one identity
does not delete other identities. No source operation has created or inspected
a real device key on this Linux host.

This follows the connector owner's fixed signer requirements in
`m-44b06bf7dfe5`. There is no arbitrary byte-signing API. The final signer bridge
must accept only the connector's typed AuthTranscript/EnrollTranscript after
verified TLS; it remains pending the frozen transport/FFI callback interface.
Private seeds and store references are not domain, view, profile, preference or
FFI data. The relevant platform primitives are Apple's
[Curve25519 signing key](https://developer.apple.com/documentation/cryptokit/curve25519/signing/privatekey),
[unlocked device-only accessibility](https://developer.apple.com/documentation/security/ksecattraccessiblewhenunlockedthisdeviceonly)
and [data-protection keychain](https://developer.apple.com/documentation/security/ksecusedataprotectionkeychain).

The five added key-vault test cases use synthetic in-memory seeds, never the
production store. They cover explicit creation/public-only output, missing-key
recovery without replacement, corrupt seeds, duplicate creation, and individual
retirement. Their source is included in both native unit-test targets. Actual
CryptoKit/Keychain and test execution are **Apple verification deferred by
operator**. Entitlement, device-lock and signing behavior remain unverified.

`CrewComponents.swift` supplies native selectable metadata/evidence and a
draft editor. Its caller supplies shared eligibility and pending state. Return
is a newline; only the explicit button submits; the view never clears a failed
or unknown draft. Dynamic strings render verbatim and copying is explicit.
`CrewClipboard.swift` performs that user-requested OS copy, with local-only
iOS clipboard items. It is not a terminal OSC clipboard handler.

These components are now composed with the real typed facade in the local
source checkpoint below. No fabricated service is used. Shared records and the live facade come from
`native-ffi`; generated sources remain its owner's responsibility. Connection,
workspace, content, action, terminal and lifecycle acceptance are still pending.

## Required remaining integration

Generated record sources from `6aa48a90` were imported byte for byte in
`35e6b0f8`, with the source owner's approval; no Rust package or root lockfile
was imported. `CrewClient/Package.swift` now declares the real generated Swift
wrapper and the separate `CrewClientFFI` binary target. Xcode targets consume
the wrapper, and `CrewUI` imports its typed records. The binary path is empty
until an actual matching artifact is staged; no empty XCFramework is created.

The explicit developer helper is
`apps/apple/scripts/install-artifacts.sh --framework <staged-framework>`.
It checks archive format, required platform slices, and exact committed ABI
headers/module map, then installs to the ignored artifact directory. It rejects
an existing destination. QA continues to use its private source copy and
staging runner instead; this helper has not installed or built an Apple binary.

`CrewDomainViews.swift` adds native lists for generated TeamData, TasksData,
MessagePage, EventPage and UsagePresentation. It preserves the supplied order
and shared current-work choice; native selection constructs the full generated
EntityKey. Dates/numbers and labels are platform formatting. Unknown usage and
context remain absent rather than zero. The local facade composition below now wires these lists and details.
Relationships, quota controls, gateway setup and native acceptance remain
required.

`CrewTerminalSurface.swift` now supplies native UIKit/AppKit SwiftTerm rendering
with platform byte/input/resize callbacks. Its UI identity is the full scope
plus attachment ID; connected composition must use generated `ClientScope`, not
an abbreviated string or raw attachment ID. Output is routed only to that
identity, input requires native focus and shared availability, and ended
renderers cannot be re-enabled. Mobile raw focus has no software keyboard;
software Return belongs to the separate explicit prompt draft. OSC clipboard
reads/writes are denied, terminal titles/directories are not OS actions, and
explicit link activation goes through the host's shared safe-link policy.
The renderer now accepts only the actual generated TerminalChunk. An ending
chunk disables input before its final data is fed; later chunks cannot reopen
that instance. Rendering keeps 2,000 scrollback lines and a 4 MiB kitty image cache, with a
steady initial cursor; these are renderer memory settings, not measured release
performance budgets. No daemon attachment or terminal workflow has run here.

The SwiftTerm package declaration pins version 1.19.0. The committed Xcode
resolution input pins its upstream tag's verified commit
`464df5207fc2432e16c9a23abe538187196daf5f`. Its
[tagged source](https://github.com/migueldeicaza/SwiftTerm/tree/v1.19.0) requires
Swift tools 6.0. Actual Swift 6 dependency resolution, UIKit/AppKit compilation
and renderer/keyboard/accessibility tests remain **Apple verification deferred
by operator**. Linux manifest evaluation describes only the root package; it
does not resolve or compile SwiftTerm.

The app must consume the real generated `CrewBindings` records and facade,
then link the matching staged `CrewClientFFI.xcframework`. No conditional import,
empty library or mocked transport substitutes for that step. Native terminal
rendering uses the approved SwiftTerm 1.19.0 pin; its upstream tag currently
identifies commit `464df5207fc2432e16c9a23abe538187196daf5f`. Byte/input/resize
adapters retain the full generated scope plus attachment ID. Raw keys are
confined to focused terminal input, and stale input is never replayed.

S01–S08/C01–C20 connection screens, typed workspace/content/evidence, safe
actions and exactly the four existing confirmations, native terminal,
foreground/background cancellation, scoped focus restoration and native tests
remain part of `native-apple-ui`. Source/manifest/parsing checks are reported
per checkpoint. Native compilation, simulator/device, focus, VoiceOver,
contrast, size/motion and signing checks remain separately labelled **Apple
verification deferred by operator**.

## Real local facade composition checkpoint

The generated files imported at `db872753` are byte-identical to the approved
local bridge `cc30064c` (`66be8de8` generation). `CrewClientWorker` now owns the
actual facade on a Swift actor. macOS explicitly discovers protected profiles
with `CrewClient.local()`; selecting one calls `connect(profileId:)`. No
socket registry, daemon facts, credentials, clock, wire parser or domain
reducer is recreated in Swift. iOS never offers or calls the Unix/local factory.

`CrewSessionModel` caches actual generated `ViewData`, context and workspace
records. MainActor publication checks the client-instance UUID and full scope;
worker calls also reject a retired instance before reaching a replacement
client. A single in-flight read loop handles bounded invalidations. One bounded
command consumer preserves raw-key invocation order; its 32 entries and 64 KiB
raw-input allowance are discarded on scope/lifecycle changes rather than
replayed. Drafts remain in memory under their captured scope. Mutations use
`plan`, exact shared confirmations and typed outcomes without optimistic
updates or automatic retry. Unknown and sent-unacknowledged outcomes remain
explicit. Ordinary closing disposes the observer and client without Shutdown.

The real Rust dispatcher supplies FIFO, bounded callbacks outside its locks.
The observer synchronously hands each short callback to the main executor,
with direct inline handling when already on the main thread. Callback return
provides backpressure; the UI handler never waits for a worker call or invokes
synchronous FFI. Queries are scheduled independently after invalidation. This
handoff was approved by the bridge owner in `m-1cd69655c0d2`.

Native root composition now includes profile/workspace selection, the five
provided projection lists, agent/task details, question sheets, explicit
message/prompt drafts, scoped evidence reads, model/CLI selection and exactly
the four shared confirmations. Connections is a per-window native sheet on
both Apple platforms, so it operates that window's actual client. Navigation
paths reset on a scope retirement, rather than addressing a replacement client
with an old entity. Gateway setup still explicitly waits for the separate
`native-gateway-bindings` factory/setup/signer exports; this partial checkpoint
is not a completed iOS application or a passing connection test.

Terminal composition requires the approved `native-attach-notice` source
correction and ordering evidence before completion. Successful Attach notices
must carry their explicit scoped attachment ID before output. The host creates
that exact renderer on the identified notice even when the actor continuation
has not yet bound the agent/panel. Later association never resets an existing
or ended renderer. Unknown chunks are discarded and counted. No first-chunk
identity inference or pre-attachment byte queue is used. The record/ABI shape
is unchanged; the producer's exact reviewed source is a separate dependency.

Available Linux source checks for this checkpoint:

```sh
nix-shell --pure \
  -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p python3Packages.openstep-plist \
  --run 'python3 apps/apple/Tests/check-source.py'
nix-shell --pure \
  -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p swift \
  --run 'find apps/apple -name "*.swift" -type f -print0 | xargs -0 swiftc -frontend -parse'
bash apps/apple/Tests/check-worker.sh
```

Project graph: 511 checks, 138 objects, six targets, two shared schemes.
Swift 5.10.1 parses all 30 files. The portable worker check emits a Swift module
from the **actual** generated Swift and C headers, then typechecks the
Foundation worker/observer adapter in Swift 5 language mode. It neither links
Rust nor substitutes fake bindings. On this older Linux compiler it reports
four Sendable warnings: UniFFI's actual generated Sendable conformances are
conditional on compiler 6, the selected Apple compiler. The Nix compiler also
reports its existing missing-glibc warning. Neither warning is suppressed or
presented as an Apple build result.

Five additional native test cases cover worker execution away from main,
instance replacement fencing, FIFO terminal end delivery, blocked-main
backpressure and an already-running callback after close/unregister. These
use an implementation of the real generated protocol only in tests; production
uses real Rust factories. Their execution, SwiftUI typechecking, linking,
Xcode builds, UI/keyboard/focus/VoiceOver, simulator/device, CryptoKit/Keychain
and signing checks remain **Apple verification deferred by operator**.
Graphs, quota/history controls, adaptive inspector/focus refinements, reviewed
Attach producer evidence and actual gateway composition remain required.

## Presentation and receipt corrections

This follow-up addresses the bounded source reviews of `8cf7b706`: designer
`m-811f944d3ff3` and reviewer `m-64141b05587d`. Composer receipt state now ties a
message ID to its exact body, scope and operation outcome. Known successful
messages retire that ID and clear only unchanged submitted text. Later edits
survive the acknowledgement; failed, unknown and unacknowledged same-body
retries keep the original ID. Slash commands requiring confirmation remain
commands; their drafts do not wait forever for a message acknowledgement.
Five native test cases cover sequential success, edits during submission,
same-ID failure/unknown retention and wrong operation/scope outcomes.

Pending/capacity invalidations no longer end a live terminal or indiscriminately
cancel its confirmation. Input-only invalidations do not reload full team/detail
history per keystroke. Terminal teardown is tied to scoped endings, relevant
outcomes, workspace/lifecycle retirement and explicit close. A second identified
Attach success cannot replace the requested operation; it is detached. Finished
attach failure shows its actual explanation and an explicit close/reopen path,
with no permanent progress indicator. Current/parked work precedes activity
and process details; limit controls explicitly say agents paused at a limit.

The producer/order authorities are now released: `cda71035` changes only the
successful Attached notice to `Some(operation_id)`; `cbc6a505` independently
proves identified-notice-before-buffered/live-byte ordering, duplicate suppression,
no-output success, failed attach, local detach and stale refusal. The latter
uses synthetic LocalOperator IO and does not claim a daemon/PTY run. Apple
source consumes that contract; unowned Rust files are not copied into this
worktree. The matching integrated library must include this source chain.

Native geometry now provides explicit List/Graph modes, Fit/Zoom and textual
relationship/dependency alternatives from the supplied TeamData/TasksData.
Largest accessibility text uses the full list path. Pushes do not refit graphs.
Native Charts show the four labelled token series and bounded shared context
samples with text alternatives; quotas show the supplied stale/reset facts.
Session objective/counts/warnings and explicit message/event filters are
rendered from generated records. Search labels describe only loaded history.
Older local pages use the core cursor. A single explicit bounded History/Events
read expands to the reviewed native retention window (1,000 messages/5,000
events), then queries that cache. v6 has no remote before-cursor field; the UI
never claims unbounded paging or repeatedly reloads an already-full window.

The approved platform safe-link adapter follows `web/src/text.tsx`, with native
absolute-host/no-userinfo restrictions from `m-180b2278cadd`. Foundation parses
only http/https with a nonempty host and no whitespace/control characters.
Detected-text punctuation trimming matches linkPieces; exact OSC targets are
not trimmed. Explicit terminal activation opens a full normalized URL preview;
a second explicit Open button invokes the OS. There is no OSC-triggered open,
clipboard read/write, shell execution or reuse of gateway trust material. Four
native test cases cover accept/reject, punctuation and no-open-without-gesture.

The latest available Linux source checks report 585 project checks / 162
objects / six targets / two schemes; all 41 Swift files parse. The actual
Foundation source check also covers composer receipts, notice routing and URL
validation, still with the four previously documented generated Sendable
warnings under Swift 5.10.1. These are not SwiftUI or Apple linking results.
The native test cases, graph/chart/layout/accessibility behavior and all Xcode,
simulator/device, focus, VoiceOver and signing checks remain **Apple verification
deferred by operator**.

Required remaining source gates: the complete reviewed
`native-action-availability` / `native-availability-core` /
`native-availability-bindings` chain, real `native-gateway-bindings` and
`native-gateway-connector` composition, adaptive inspector/focus work and final
native source tests. The availability query will use the full captured intent;
no mutating plan probe or Swift grant allowlist substitutes for it. The current
quota controls explicitly remain unavailable until this result is wired.
This is a checkpoint of an in-progress card, not whole-application acceptance.

## Adaptive detail and focus source

Native detail now uses the platform inspector column on a wide, regular
window with readable text size. Width is measured by an outer GeometryReader,
so opening the inspector cannot turn its own content-width measurement into a
collapse/reopen loop. Compact width and accessibility text use the existing
NavigationStack path. Resizing hands the same complete EntityKey/ResourceKey
to the appropriate container without refetching or readdressing another
entity. macOS exposes Show Inspector in the View menu; both platforms retain
the options-menu control and the detail's explicit Close action. No visibility
preference or selected entity is persisted across app sessions.

A per-window focus coordinator restores a still-rendered original opener,
then the closed entity in the current surface, then the current destination's
own native control. IDs are full generated EntityKeys, never raw agent/task
strings. Native lists/graphs expose selection separately from keyboard and
VoiceOver focus, and scroll the chosen row into the viewport. A matched focus
request is consumed once, so a later layout or push cannot replay it; scope
retirement cancels it. Requests do not move focus while a modal editor,
confirmation or settings sheet is active. Invalid signed event sequences
cannot become a fabricated event-zero focus key. Four added native test cases
cover restore ordering, disappeared/retired identity, consumption and independent
window inspector state. Their execution remains deferred.

Reviewer follow-up `m-b918bbc3dd7b` is also addressed: attachment notSent and
canceled outcomes with the established full-scoped attachment ID disable the
renderer. The additional native routing test covers both cases. While a
terminal exists, a pending/availability-only notice may also come from another
Send/Answer operation. The typed receipt/outcome path still updates the
operator's send draft, older-read state and feedback; only full projection
refresh is deferred until a domain change. The shared availability query will
remain independently required before actionable control completion.

Current structural check remains 585 checks / 162 objects / six targets / two
shared schemes. All 42 Swift files parse on the pinned Linux compiler. These
source changes add no dependency, generated-byte edit or Rust/root change.
Native inspector sizing, focus/VoiceOver, keyboard, adaptive handoff and all
Apple compilation/UI execution remain **Apple verification deferred by
operator**. Availability and gateway composition are still required source
work, rather than passed or optional acceptance rows.

## Public setup presentation preparation

`CrewPublicProfileForm.swift` and `CrewEnrollmentViews.swift` prepare the
approved S02–S05 reading order with native public fields, full fingerprint
review, independent-check acknowledgement, device-label/storage explanation,
explicit host-console enrollment instructions and accepted-code group layout.
`CrewAccessDisclosure` includes the complete separate Read/Control/Terminal
and no-OS-sandbox wording. These are presentation components only: they receive
bindings, supplied public values and callbacks. They neither parse a profile,
create a key/grant/attempt, fabricate comparison data, predict acceptance nor
implement any handshake. No trust failure bypass or phone-side Approve button
is supplied.

The components are deliberately not yet mounted into a simulated flow.
`native-gateway-bindings` must supply the actual trusted-profile factory,
sealed-transcript signer replies and setup/session ports first. The connected
composition will bind only its real results, with per-instance/handle fences
and ordered callback delivery. Current local source and these prepared views
do not certify an iOS remote application.

All 44 Swift files parse with the pinned Linux compiler; project validation
remains 585 checks / 162 objects / six targets / two schemes. No actual Apple
SDK/typecheck, setup, Keychain, enrollment, device, simulator or accessibility
run is claimed: **Apple verification deferred by operator**. The full reviewed
availability source/API/FFI chain and real gateway composition remain required
before this card can be submitted.

## Reconnect draft retention and accessible presentation

The graph picker now displays the same effective List state that the renderer
uses at accessibility text sizes, with a readable explanation. The previous
graph preference can return at a readable size; the UI does not label a forced
list as Graph.

Per-window draft text uses endpoint/workspace/purpose identity for retention,
while every action, receipt, confirmation and attachment still carries the
complete current ClientScope. Reopening the same authorized recipient after
ordinary reconnect therefore reveals retained text. An explicit send under the
new live scope retires the old operation association and preserves a same-body
message ID for safe deduplication; the planner still validates the complete
current scope and authority. Nothing is automatically dispatched or replayed.
Old-scope outcomes cannot clear the reopened text. Learned workspace revocation
purges that workspace's drafts; endpoint sign-out purges its endpoint's drafts.
Two native source cases exercise ordinary reconnect visibility versus revoke
purge, and same-body retention with retirement of the old operation identity.
They remain **Apple verification deferred by operator**.

Available checks still report 585 structural project checks / 162 objects /
six targets / two schemes; all 44 Swift files parse. The actual Foundation
worker/receipt/routing/link sources typecheck against the generated bindings.
This checkpoint does not change the required complete availability/FFI and
gateway source gates or claim an Apple build, setup, device or UI pass.

The evidence route is now tracked separately from its cached ResourceState.
Native Back below its recorded stack depth closes that route. A layout transfer
keeps only an actually open route, so Transcript → Back to agent → widen does
not resurrect the transcript. Switching to another destination in wide mode
normalizes its retained compact stack before presenting the inspector; each
selected entity stays scoped without two simultaneous detail surfaces. Three
additional navigation source cases cover Back, open-route transfer and the
second destination's normalization. They do not claim an executed Apple UI
sequence.

## Identified send receipts across the barrier

The reconnect receipt path follows the accepted A07 rule (`m-f1f49bcc8be4`,
`m-2fc64e643973`). The shared Core may hold an identified Send and replay it
once after a newer snapshot barrier. It rebinds the held proposal to the newly
verified canonical scope, so the final notice carries the **original operation
ID with the new attempt's scope** (`real_core_lifecycle.rs:157-158`). UI receipt
correlation is therefore the client instance (the model's instance fence),
endpoint, workspace and operation ID. It never requires full scope equality or
the old attempt. Action dispatch continues to use the complete current scope.

The submitted message is an immutable receipt, separate from the editable
draft, and everything known about it is stated on that receipt rather than
beside the editor. While the Core holds it, the composer shows the original
body and recipient read-only, states that it is still pending after
reconnect, and says that closing the sheet does not cancel it; the disabled
Send states "Send after the submitted message above resolves." Later edits
are a separate draft. The wire message ID is not displayed. The UI neither
replans nor resends. Only a matching final notice resolves the receipt.
Success with the exact submitted text still in the editor clears both and
says "Message sent."; success after later edits is shown on the receipt
("Last submitted message", Sent.) and leaves those edits untouched. Unknown,
the other non-success outcomes and Core refusals before dispatch keep the
body and show their reason on the receipt. Mismatching endpoint, workspace
or operation notices are ignored. Receipt status changes are posted as
VoiceOver announcements. So is the editor's own note (such as "Message sent."), which
the next edit removes. The compose, prompt and question sheets scroll, and a
long submitted body is bounded inside the receipt.

Learned revocation, workspace removal and sign-out purge the draft and its
receipt, because the Core retires the held retry and no notice will arrive.
Client replacement and a workspace change retire a pending receipt as
**unknown** instead: the message may already have been delivered, so it stays
visible ("Outcome unknown after the connection changed…"), keeps its ID so an
unchanged explicit retry still deduplicates, and no explanation is placed
beside later edits. A draft with nothing pending is unaffected by retirement.
A separate bounded receipt history handles an outcome that arrives before the
actor's plan continuation, including a scope change during that continuation.
The generic view/terminal paths keep their full-scope fences.

Native source cases: `CrewSendDraftTests` covers new-attempt-scope success
with the original operation ID, success shown on the receipt beside later
edits, held Unknown retention, a refusal before dispatch on the receipt,
retirement as unknown with ID retention, and retirement leaving unsubmitted or
already-resolved drafts unchanged. `CrewClientWorkerTests` covers the model
sequence Dispatch → connection loss → new attempt → reopened composer →
new-attempt-scope success (exactly one plan call, later edit kept), held
Unknown retention, revocation purge with a later notice ignored, and client
replacement retiring the receipt as unknown without touching other drafts.
Their execution remains **Apple verification deferred by operator**; the
Foundation-only test sources typecheck on Linux (below). This is not a Rust
replay or Apple application run.

Restored evidence routes also activate their captured entity/read under the
current scope, rather than showing a perpetual loading state after another
destination cleared the display cache. A newly active wide destination always
normalizes its physical inspector path, including one with no selection;
another destination's saved open route is not treated as Back.

## Shared action availability

The generated `CrewBindings.swift` from the reviewer-approved
`native-availability-bindings` release `75173b46` (sha256 `db9b3632…`) was
imported byte for byte in `e1b8fc32`; the C header and module map are
unchanged. Its source chain is `5165ff56` (`c37bff6f` producer preflight),
`a4252ec4` (`f3d73b16` Core query) and `75173b46` (FFI), which QA lands
together.

Controls now take eligibility and the disabled reason from the shared Core's
read-only `ViewQuery.availability(intent:)` for the **exact** intent they
would submit: message Send (including its deduplication ID), terminal prompt
submission, question answers (the typed draft and each suggested answer),
model/CLI choices with the current note, and the workspace-wide Continue,
Restart and Stop team controls in the options menu and the usage view.
Agent detail actions keep the Core's per-agent projection availability.
Each control registers its intent while visible; one in-flight worker read
answers every registered intent in the current scope and is repeated after
any shared notice or new input. A malformed intent fails only its own entry
with the same `ClientError` that `plan` returns; a closed client fails the
read. The reason shown is the Core's canonical `ClientReason.message`. An
intent captured under an earlier connection attempt is never queried: it
says "Reopen this target from the current workspace connection." Each
control registers under a stable name; while the answer for edited input is
outstanding it keeps that control's previous answer, so typing does not
flash the button or its reason. A control with no answer yet (just opened, or
after a scope change) is disabled without a reason line. The planner still
revalidates the exact submitted intent, and a refusal lands on the receipt.
Queries follow each keystroke; if device testing shows typing lag, debounce
belongs in the view modifier, never in the model. No mutating plan probe, grant
list or Swift policy decides eligibility, and execution still revalidates
through `plan`. The Send availability intent and the dispatched intent are the
same value: the draft keeps one stable candidate ID until Send uses it.

Native source cases in `CrewClientWorkerTests`: Core answers for exact
intents (unavailable with reason, available, malformed), re-reading after a
shared notice, no query for an earlier attempt's intent, no plan call, and the
composer's queried intent equalling the planned intent. A further case keeps a
control's last answer while its edited intent is checked and forgets it when
the control disappears. `CrewSendDraftTests`
covers candidate ID stability and its use by Send. The worker read typechecks
on Linux against the released bindings; the SwiftUI composition and the
model cases remain **Apple verification deferred by operator**.

## Native test sources

Each source below belongs to both the `CrewMacTests`/`CrewIOSTests` unit
bundles, except the UI-test source, which belongs to both UI-test bundles. None
has been executed: **Apple execution deferred by operator**.

| Source | Cases | Linux evidence |
| --- | --- | --- |
| `Tests/CrewSendDraftTests.swift` | 12 | typechecks with its host source and corelibs XCTest |
| `Tests/CrewNoticeRoutingTests.swift` | 5 | typechecks with its host source and corelibs XCTest |
| `Tests/CrewExternalLinksTests.swift` | 4 | typechecks with its host source and corelibs XCTest |
| `Tests/CrewClientWorkerTests.swift` | 13 | parses; the model cases need SwiftUI/Observation |
| `Tests/CrewShellTests.swift` | 13 | parses; needs SwiftUI |
| `Tests/AppleDeviceKeyVaultTests.swift` | 5 | parses; needs CryptoKit/Security |
| `Tests/CrewShellUITests.swift` | 2 | parses; needs an Apple UI-test runner |

`bash apps/apple/Tests/check-worker.sh` emits the actual generated
`CrewBindings` module, typechecks the Foundation worker/receipt/routing/link
host sources, then typechecks the three Foundation test sources together with
those host sources against the pinned Swift 5.10.1 corelibs XCTest. On Linux the
`@testable import CrewMac`/`CrewIOS` lines are compiled out, so the tests and
host sources share one module. Tests are typechecked, not run: running them
needs the matching Rust FFI library, and no Apple target is built. Swift
5.10.1 reports six Sendable warnings in `CrewClientWorker.swift`: the four
documented above plus `ActionIntent` and `ActionAvailability` in the
availability read. All six are UniFFI conformances generated under
`#if compiler(>=6)`, the selected Apple compiler; none is suppressed.

## Deferred Apple-run checks recorded from review

These are source-reviewed but need the deferred Apple run:

- Compose Send stays reachable at accessibility Dynamic Type (AX5), at iPhone
  SE size, and with the keyboard up; the receipt status is never truncated.
- `AccessibilityNotification.Announcement` resolves with `import
  Accessibility` (added) and VoiceOver hears receipt changes and "Message
  sent." once each.
- Typing in compose, prompt, answer and model-note fields shows no lag from
  the per-keystroke availability query; debounce in the view modifier if it
  does.
