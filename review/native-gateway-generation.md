# Native workspace and grant generations

Task `native-gateway-generation`, following the verified route proof in
`759aea95`. Connector integration exposed a server defect after a host-side
grant edit: the filtered listing replaced `WorkspaceEntry.generation` with
the device's grant counter, while `GatewayRoute.workspace_generation` correctly
retained the immutable membership generation. The strict connector therefore
refused rebinding when the listing said 2 and the verified route said 1.

The server now preserves the member entry's generation while filtering the
listing. `WorkspaceGrant.grant_generation` remains the separate authority
revision in both listing and opened metadata. The per-device visible cache
already compares grant records, so a grant revision still increments the
listing revision even when membership identity is unchanged. No record/wire,
v6, route-proof, permission-policy or client-validation change is needed.

The real isolated regression enrolls Read, opens a workspace, grants Control,
checks old-channel closure with 4102, and compares the updated listing and
verified route. It then revokes the workspace grant, checks closure and denial,
regrants Control, and opens Workspace and Action purposes with the unchanged
membership/daemon identity and a greater independent grant revision. Snapshot
and ping replies verify that the new channels remain usable.

Before the correction, the exact regression fails at the updated listing:
generation 2 differs from captured membership generation 1. The local log is
`target/native-generation/before.log`. Tests use temporary runtime/state/config
paths and a loopback-only TLS listener; all inherited Git and Crew/Herdr agent
context is removed. No root repository, operator service or real device is
changed. Regrant discovery waits up to five seconds for its asynchronously
restarted summary watch; the identity assertions do not accept missing data
or a fabricated generation while discovery catches up.

The corrected complete real-daemon/TLS gateway suite passes 19/19, including
the new edit/revoke/regrant case. Native unit/policy tests pass 11/11. Feature
all-target clippy with `-D warnings`, formatting and the actual Rust/Cargo
1.82.0 locked default CLI build/help gate pass. Cargo manifests, lockfile,
protocol records, browser code and client-validation behavior are unchanged.
Local output is preserved in `target/native-generation/`: baseline failure,
native-unit output and corrected complete-suite output. The independent
connector regression remains its owner's unchanged acceptance check.
