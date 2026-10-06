# Shared client adoption in the terminal dashboard

The terminal dashboard uses the real `crew_client::ClientCore` for domain
reduction, bounded histories, question and answer state, scope retirement,
command eligibility, operation identifiers, confirmations and the permitted
identified-message retry. Its request encoder and timeouts come from the shared
command and policy modules. No second production daemon-command implementation
remains in the terminal adapter.

`crates/crew-tui/src/shared.rs` owns the single UI-thread Core and translates
existing terminal commands and daemon events. The public App data fields remain
derived render caches. Each relevant ChangeSet domain refreshes its own cache;
unrelated pushes do not replace it, and production caches never write back to
Core. Rust-only immutable getters expose canonical retained records and server
time, including the wide snapshot clock offset. The terminal's 500-message and
2,000-event history capacities and N1 completed-work ordering remain intact.

Navigation, selection fallback, focus, row measurement, animations, model display
names, terminal controls and locale formatting stay in the terminal interface.
Current-work, satellite ordering, task layers and slash-command metadata use the
shared implementations. The existing public const DASHBOARD remains consumable
from a downstream const function with Rust 1.82.

## Local transport and lifecycle

The subscriber authenticates and subscribes first, then fetches a snapshot over
a second authenticated connection. Buffered overlapping pushes remain visible;
this keeps the original TUI barrier semantics. Physical subscription epochs fence
old snapshots, pushes and disconnects. Every planned dispatch verifies the
captured daemon PID, startup time and workspace on its request connection, then
checks the current full scope again immediately before writing.

The fast, lifecycle and terminal workers each have a 64-command queue. A terminal
burst captures full scope and agent, coalesces only that same target, splits at
UTF-8 boundaries within 64 KiB, and asks the UI thread to plan the burst. It uses
AgentInput with submit=false and waits for the daemon's actual acknowledgement.
Submitted prompts retain their own duplicate guard; pending raw input and a
pending prompt do not block each other. Written input is never replayed. Retired
or failed input discards its remaining captured bytes and reports a visible
failure. Queue rejection is fed to Core as NotSent rather than leaving a pending
operation without a completion.

All four destructive dialogs capture Core confirmation tokens when opened;
answering or closing the dialog confirms or cancels those immutable identifiers.
Only Core final outcomes update the result presentation. A held identified Send
produces no premature final status, and its deadline uses the explicit monotonic
clock even if the wall clock changes.

Shutdown latches stop, interrupts registered connections through close-only
handles, stops workers waiting for UI planning or queue input, and joins owned
threads. Registration closes immediately if stop won the race. The terminal is
restored before host cleanup. Token reads and a Unix connect still in progress
precede registration and cannot be interrupted by this handle; the tested bound
applies to registered socket IO and worker waits. Detaching closes client channels
and leaves the daemon running.

## Verification

Run tests with repository override and crew agent environment variables removed.
The final source is checked with locked client/TUI tests, the unedited render
suite, strict Clippy, formatting, the declared Rust 1.82 compiler, and the default
CLI compatibility gate. Dedicated regressions exercise stop/register races,
held hello and write outcomes, a worker awaiting UI planning, retirement during
proof reads, stale subscription epochs and UTF-8/FIFO boundaries. Shared state
vectors pass through the actual App/Core adapter rather than a test engine.

The isolated live journey builds the actual CLI, starts a scripted PTY agent in
temporary runtime/state/config directories, and drives the production subscriber,
UI/Core and request workers. It checks current work, PTY output, the actual raw
AgentInput path, question answers, identified Send and an immutable kill token:

```sh
./scripts/build-env cargo build --locked -p crew --bin crew
env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_COMMON_DIR \
    -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES \
    -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
    CREW_TUI_TEST_BINARY="$PWD/target/debug/crew" \
    ./scripts/build-env cargo test --locked -p crew-tui --lib \
    net::live_tests::shared_terminal_adapter_against_a_real_daemon \
    -- --ignored --nocapture
```

The live test is explicitly ignored during ordinary workspace runs because it
requires that separately built CLI path; the command above executes it. It
does not use or restart an operator daemon. Apple verification remains deferred
by the operator; this Linux validation makes no Apple compilation claim.

Daemon scheduler and parking source, protocol versions, web source, generated web
policy, bundle and render-test bytes are unchanged by this adoption.
