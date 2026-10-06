# Apple implementation status

The approved navigation foundation is `4479a301`. Substantive implementation
uses the reviewed N2A source at `a159bd03` and the connection experience at
`5015460a`. Source now composes the real local facade on macOS. Gateway composition is
still required for iOS; source preparation and unperformed native execution
remain distinct.

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
