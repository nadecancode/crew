# Apple implementation status

The approved navigation foundation is `4479a301`. Substantive implementation
uses the reviewed N2A source at `a159bd03` and the connection experience at
`5015460a`. The app currently remains unconnected; this document distinguishes
source preparation from working client behavior.

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

These components are prepared for the real typed views. They are not currently
wired into a fabricated service. Shared records and the live facade come from
`native-ffi`; generated sources remain its owner's responsibility. Connection,
workspace, content, action, terminal and lifecycle acceptance are still pending.

## Required remaining integration

`CrewTerminalSurface.swift` now supplies native UIKit/AppKit SwiftTerm rendering
with platform byte/input/resize callbacks. Its UI identity is the full scope
plus attachment ID; connected composition must use generated `ClientScope`, not
an abbreviated string or raw attachment ID. Output is routed only to that
identity, input requires native focus and shared availability, and ended
renderers cannot be re-enabled. Mobile raw focus has no software keyboard;
software Return belongs to the separate explicit prompt draft. OSC clipboard
reads/writes are denied, terminal titles/directories are not OS actions, and
explicit link activation goes through the host's shared safe-link policy.
Rendering keeps 2,000 scrollback lines and a 4 MiB kitty image cache, with a
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
