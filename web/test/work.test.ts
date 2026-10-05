import { expect, test } from "vitest";
import { workOf } from "../src/format";
import type { Task } from "../src/protocol";

function task(id: string, status: Task["status"], started_at: number | null, completed_at: number | null = null): Task {
  return { id, title: id, description: "", status, assigned_agent: "a", dependencies: [], reviewer: null, created_at: 0, started_at, completed_at, progress: null, note: null, result: null, timeout_secs: null, artifacts: [] };
}

const cases: { name: string; tasks: Task[]; id: string | null; done?: boolean; elapsed?: number | null }[] = [
  { name: "resumed older Working beats newer Waiting", tasks: [task("A", "working", 100), task("B", "waiting", 200)], id: "A" },
  { name: "Working beats newer Review", tasks: [task("A", "review", 300), task("B", "working", 200)], id: "B" },
  { name: "latest started Waiting wins", tasks: [task("A", "waiting", 100), task("B", "waiting", 200)], id: "B" },
  { name: "Waiting ties keep first board entry", tasks: [task("A", "waiting", 100), task("B", "waiting", 100)], id: "A" },
  { name: "started Blocked beats Waiting", tasks: [task("A", "blocked", 100), task("B", "waiting", 200)], id: "A" },
  { name: "unstarted Blocked yields to Waiting", tasks: [task("A", "blocked", null), task("B", "waiting", 200)], id: "B" },
  { name: "unstarted Blocked yields to last Done", tasks: [task("A", "blocked", null), task("B", "done", 1, 50)], id: "B", done: true },
  { name: "Working with unknown start beats Waiting", tasks: [task("A", "working", null), task("B", "waiting", 200)], id: "A", elapsed: null },
  { name: "Waiting beats newer Review", tasks: [task("A", "review", 300), task("B", "waiting", 200)], id: "B" },
  { name: "started Ready remains a display fallback", tasks: [task("A", "ready", 100)], id: "A" },
  { name: "unstarted Ready and Queued are not current", tasks: [task("A", "ready", null), task("B", "queued", null)], id: null },
  { name: "latest completion wins among Done", tasks: [task("A", "done", 1, 10), task("B", "done", 2, 20)], id: "B", done: true },
  { name: "Failed is ignored", tasks: [task("A", "failed", 100), task("B", "waiting", 50)], id: "B" },
  { name: "other agent's work is ignored", tasks: [{ ...task("A", "working", 100), assigned_agent: "z" }, task("B", "waiting", 50)], id: "B" },
  { name: "unknown start preserves unknown elapsed", tasks: [task("A", "working", null)], id: "A", elapsed: null },
  { name: "elapsed measures the chosen executing card", tasks: [task("A", "working", 400)], id: "A", elapsed: 600 },
  { name: "multiple Working snapshots keep latest start", tasks: [task("A", "working", 100), task("B", "working", 200)], id: "B" },
  { name: "zero is a known Blocked start", tasks: [task("A", "blocked", 0), task("B", "waiting", 200)], id: "A" },
  { name: "status priority does not depend on timestamp magnitude", tasks: [task("A", "working", -100), task("B", "waiting", 3e15)], id: "A" },
  { name: "Waiting without start keeps first board tie", tasks: [task("A", "waiting", null), task("B", "waiting", null)], id: "A", elapsed: null },
];

test.each(cases)("workOf: $name", ({ tasks, id, done = false, elapsed }) => {
  const work = workOf(tasks, "a", 1000);
  if (id === null) expect(work).toBeNull();
  else {
    expect(work).toMatchObject({ task: { id }, done });
    if (elapsed !== undefined) expect(work?.elapsed).toBe(elapsed);
  }
});
