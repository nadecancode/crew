# Native listing coherence during membership discovery

Task `native-gateway-listing`, following approved `589dca0c` and `17d071cb`.
The strict connector's extended revoke/regrant journey exposed an orphan grant:
the persistent registry already authorized the workspace, but its asynchronous
summary watch had not restored the visible member. The server sent one grant
with no joined entry, so the connector correctly refused the malformed index.

The production correction is confined to `native/service.rs`: publish the
grant inside the same block that publishes its corresponding visible member.
Both vectors therefore have the same workspace ids in the same order. A grant
remains durable and authorized while discovery is pending; only its listing
metadata waits for a member. The existing visible cache publishes a new
revision when that member and grant appear. Membership generation remains
separate from the grant revision; no entry, daemon incarnation, permission or
client authority is fabricated or relaxed.

The deterministic service regression uses the real protected store and TLS
identity loader in a temporary directory, without binding a listener. It
commits a grant through `change_grant` while the member is absent and checks
empty joined/grant vectors with an unchanged revision. It then supplies the
cached member, checks exactly one paired entry/grant and revision +1, removes
the member, and checks its eventual reappearance. Repeated unchanged reads do
not advance the revision. Against the prior listing construction this test
fails at `absent.grants.is_empty()`; the failure is retained in
`target/native-listing-coherence/before.log`.

The real TLS/daemon test client now checks every received listing reply and
unsolicited listing push before request correlation, including frames skipped
while awaiting another reply. Grant ids must equal joined ids in order and
`others` must be empty. Thus the existing actual grant-edit/revoke/regrant
journey cannot hide malformed intermediate listings inside its bounded
discovery wait. It still checks higher grant revisions, stable membership and
verified route identity, old-channel closure with 4102, denied removed access,
and usable fresh Workspace/Action channels. The connector owner's extended
real regrant/select journey remains the independent consumer acceptance.

Validation on this Linux host:

- Native unit/policy tests: 12/12.
- Complete real TLS/daemon gateway suite: 19/19.
- All-target clippy with `-D warnings`, gateway feature enabled and disabled.
- `cargo fmt --all --check`.
- Actual Rust/Cargo 1.82.0 locked default CLI build and help checks.

Output is retained under `target/native-listing-coherence/`. Test invocations
remove inherited `GIT_*`, `CREW_*` and `HERDR_*`; real services use only isolated
temporary runtime/state/config paths and explicit loopback TLS listeners. The
compiler compatibility check reuses the scheduler's read-only toolchain with
this worktree's separate target directory. No active operator service, root
repository or real-device state is changed. Protocol/v6 records, route proof,
client code, legacy web sources, Cargo manifests and lockfile are unchanged.
Apple verification remains deferred by operator.
