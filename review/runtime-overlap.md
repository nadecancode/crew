# An agent waiting on a teammate takes its next task: review (agent: reviewer)

Reviewed: the `runtime` branch at `a700f29e` (commits `db495f21` to `a700f29e`), on top of the planning
guidance `c2d31596` and the fixture repair `5e639b57`; with it the display change `fb0c3a66` and
`b11dd5a9` (`graph`) and the tests `97de7b68` (`qa`). Against `main` at `15de7cf`.

**Verdict: approved.** The findings below were raised on earlier commits of the branch and are closed.

## What changes

Until now a task its agent had reported as waiting on a teammate still occupied that agent, so the
agent's next ready task sat undelivered until the teammate answered, and nothing said so. The operator
asked for the two to overlap. Now:

- A waiting task is *parked*: it keeps its owner but not the owner's attention. Once the turn that
  parked it has ended, the agent's next ready task is dispatched. At most one task per agent is being
  worked on (working, or blocked after it was started); any number can be parked.
- Calls that name no task (`task_get`, `task_update`, `task_complete`, `agent_status`, the task a commit
  is attached to) mean the task being worked on; if there is none, the parked task started last. A
  parked task is reached by its id. Resuming one while another is being worked on is refused, with a
  message that says what to do instead.
- Messages go on being delivered while the agent works on something else. A teammate's or the
  operator's message leaves a note against the parked task (kept across a daemon restart, and after
  the inbox was read), and when the agent is free again the parked task is resumed before any new one,
  with the ids of the messages that arrived for it. An answer that arrives while the agent is doing
  nothing else resumes the task in the same delivery, as before, without a second notice.
- A dependency still waits for a finished, approved task; a review still gates what depends on it; a
  task blocked after it was started still holds its agent. Parked time does not count against a task's
  timeout.
- The wire protocol and the task statuses are unchanged. `agent_status` gains an optional `task_id`.
  The dashboard and the TUI show the task being worked on as the agent's current work and list parked
  ones with their own status (`web/src/format.ts`, `crates/crew-tui/src/app.rs`).

## Found in review, closed

- **R-RT-1** (blocker): the first version withheld a ready task from *any* agent whose turn was marked
  running. An agent restarted fresh never got its task back; the existing test
  `a_fresh_restart_hands_its_task_to_the_new_session_whatever_the_old_one_said_last` timed out (it passes
  on the base). Now only an agent that holds a parked task waits, and only for the turn that parked it;
  that mark is cleared wherever a turn ends, the session is stopped, exits or hits a limit.
- **R-RT-2** (blocker): the same mark counted a blocked agent as activity, so a team stuck on a blocked
  agent was never reported as stalled. A probe I added to the scheduler's tests returned no stall. The
  two are separate now, and a test builds the planner's view the way the daemon does.
- **R-RT-3** (major): with a single waiting task and nothing else to do, an arriving answer no longer
  resumed the task on delivery; it took a separate notice and sometimes a second turn. Restored.
- **R-PRES-1** (major, display): the first form of the display rule left 31 bytes of script under the
  ceiling. The final form leaves 85.
- Smaller: message metadata no longer rewritten from null to an empty object; a rework returned to an
  author with only parked tasks goes straight to work; notes for tasks that are finished are dropped;
  the reply of `agent_status` is one line again; a teammate who happens to be called `scheduler` can
  answer.
- A new unit test that restarts a daemon in the test process failed once in five of my full runs (the
  second daemon found the first one's lock). The author found it independently and moved the test into
  its own process. On the final head the library suite passed six times in a row.

## Own runs, on `a700f29e`

- `cargo test -p crew --lib`: 478 of 478, six runs in a row.
- `cargo test -p crew --test team` (the existing daemon tests, untouched): 86 of 86, the restart test
  among them in under two seconds.
- `cargo test -p crew --test overlap` (new): 8 of 8. They drive a real daemon with held agents: an
  owner parks a task and receives the next; the answer arrives during the other task, is read there, and
  is still named when the parked task resumes; both finish once and are dispatched once; calls without
  an id reach the task being worked on; a commit made during the second task is attached to it and not
  to the parked one; three parked tasks resume newest first; parking again uses up the note and no
  further notice comes until a new message does; a parked prerequisite releases nothing; a fresh restart
  hands every open task to the new session.
- `crates/crew-tui` render tests at `fb0c3a66`: 60 of 60; with the old rule two of the three new ones
  fail.

Not run by me: the workspace suite, `cargo fmt` and `cargo clippy` (the author's: 964 passed, 0 failed, 2 ignored, read from its log; clippy with warnings denied and fmt clean). The
sandbox tests skip themselves where I run.

## The dashboard for it

Bundle `9d2e8387` (`qa`): source `fnv1a64:f22f3cf5ad74d25f`, byte for byte my own build and a rebuild
of its tree. Script 230,415 (85 under), CSS 33,906 (94 under), terminal chunk 332,915, bundle 597,922
(2,078 under). `bun run test` 197 of 197; `mock` and `perf` 229 of 229 on a quiet host (first paint 72
ms at most, live 153 ms at most, switch 49 ms at p95, nothing while idle). The 20 cases of
`web/test/work.test.ts` and the four of `web/e2e/parked-work.spec.ts` fail on the previous bundle (9 and
4) and pass on this one.

## Known, left as it is

- **Nothing checks the work tree when a task is parked.** `git_commit` commits everything, and a commit
  is attached to the task being worked on. The agent's instructions now say to commit before parking, or
  to keep track of what belongs to which task; an agent that does neither will have the parked task's
  edits swept into the next task's commit.
- An unscoped message is offered to every parked task of its recipient, one after the other; each costs
  a turn if the agent parks it again. It is bounded (once per task per message) and tested.
- A parked task cannot be swapped in while another is being worked on; the agent finishes or parks the
  current one first. What depends on the parked task waits meanwhile.
- A running team keeps the binary it started with: this takes effect in a team started with the new
  build.
- Stopping an agent and restarting it with its session: read in the code and the README, not run by me.
