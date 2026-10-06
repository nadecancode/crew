# Gateway restart followed by a workspace revocation

Card: `native-gateway-restart-revoke`. Report: a device granted workspaces
alpha and beta, after a gateway restart and reconnect on alpha, saw alpha
revoked and then stayed `Reconnecting` with no retry; a manual reconnect then
signed the device out although beta was still granted.

## Root cause

Both symptoms come from the client driver (`crew-client-transport`); the
gateway server behaves as its contract (`notes/native-gateway.md`) requires.

1. A scoped gateway session has two kinds of channel: the endpoint index
   (device authority and the filtered listing) and the selected workspace
   route. `connect` opens the index; `select_workspace` adds the route. A
   reconnect (automatic or explicit) went through `start_selection`, which
   with a selected workspace opened only the route. After the restart the
   session therefore had no index. When the host then revoked alpha, the
   route ended with `GrantChanged`; with no index left, the endpoint was set
   to `Reconnecting`, and `GrantChanged` correctly schedules no automatic
   retry (an access change must not silently rebind), so the session stalled.
   Without a restart the original index was still open, so the same
   revocation only ended the route and the index listed beta.
2. A manual reconnect retried the still-selected alpha route. The gateway
   refuses a route to an ungranted workspace with the same generic
   `unauthorized` answer it uses for any pre-route refusal; the connector maps
   it to `Authentication`. Again with no index, the driver treated that as a
   device sign-out.

The server half was checked directly: after a restart the same key
authenticates the index and a route, a workspace revocation closes only that
route (4102), the open index pushes a newer listing without alpha, a fresh
index authentication succeeds, alpha's route is refused with `unauthorized`
and beta's route opens. No server change is needed.

## Fix

`crates/crew-client-transport/src/driver.rs`:

- A selection records that its endpoint uses an index once the index
  authenticates or a `ScopedGateway` route opens. For such a selection every
  reconnect reopens the index first (as on the initial connect) and then the
  selected workspace under a later attempt, so the route binds under the
  index's endpoint attempt.
- While that index is open, a gateway refusal of the route
  (`Authentication` or `Permission`) marks only the workspace
  `AccessRevoked`; the index alone decides whether the device signs out.

Unchanged: grant, generation and route validation; `GrantChanged` still
schedules no automatic retry; local connections never open an index and keep
their sign-out mapping.

## Evidence

Before-control (driver from main `2cb18b2b`, identical on `fe0a7813`;
`target/native-validation/restart-revoke-before.log`):

- `real_gateway_restart_then_workspace_revoke_keeps_the_device_and_its_other_workspace`
  fails: after the revocation the endpoint is `Reconnecting`, not
  `Authenticated`.
- `gateway_reconnect_reopens_the_index_beside_the_selected_workspace` fails:
  the reconnect opens only the workspace route.
- `gateway_route_refusal_with_index_keeps_the_device_signed_in` fails: the
  refused workspace shows `Reconnecting`.
- `local_route_refusal_still_signs_out_without_opening_an_index` passes
  before and after (local behaviour kept).

After the fix (`target/native-validation/restart-revoke-after.log`, main
`fe0a7813` plus this change):

- New real TLS and daemon regression in `crew-client-transport`
  (`tests/gateway_daemon.rs`): two daemons, enrollment, Control grant change,
  gateway restart, reconnect, alpha revocation with the device still
  authenticated and beta listed without a manual reconnect, then an explicit
  reconnect whose alpha route is refused without signing out, and beta opens.
- New server regression in `crew` (`tests/native_gateway.rs`):
  `workspace_revoke_after_restart_keeps_the_device_index_and_remaining_workspace`.
- Transport with `gateway-tests,real-core-tests`: 21 unit, 4 attachment,
  19 driver, 7 gateway daemon, 6 local daemon, 2 local negative,
  1 real core lifecycle passed; with `journey-tests` and a feature-built
  `crew` binary, all 10 gateway daemon tests including the 3 PTY journeys
  passed; `runtime` alone passed.
- `crew` `native_gateway` with `native-gateway`: 20 passed.
- `crew-client-ffi` `bindgen,gateway-tests`: 41 passed, including the live
  gateway facade journeys and generated-output freshness.
- `crates/crew-desktop/tests/run-window-test.sh --gateway`: the live GTK
  gateway journey passed and listed the remaining workspace after the
  revocation without taking its manual-reconnect fallback.
- Strict Clippy for the transport (`journey-tests`, `runtime`, default),
  `crew` with `native-gateway` and the FFI with `bindgen,gateway-tests`;
  `cargo fmt --all --check`; `cargo metadata --locked`.
- Rust 1.82.0 locked build of `crew`; its version and help commands run.
