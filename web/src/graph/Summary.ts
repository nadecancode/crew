import { badge, doing, fmtElapsed, progressOf, runningLabel, runtimeGlyph, workLine, workOf } from "../format";
import type { Agent, Task } from "../protocol";

export type EntrySummary = { name?: string; status?: string; tone?: string; title: string; info: string; detail: string };

export function agentSummary(a: Agent, tasks: readonly Task[], now: number, asking = false): EntrySummary {
  const work = workOf(tasks,a.id,now), progress = progressOf(a);
  return {
    name: `${runtimeGlyph(a.runtime)} ${a.id}`, status: a.paused ? "paused" : a.status, tone: `rt-${a.runtime}`,
    title: workLine(a,work).text,
    info: `${a.runtime} · ${runningLabel(a) ?? "model not reported"} · ${doing(a,work,now).text}`,
    detail: [a.context ? `Context ${a.context.percent}%` : "Context not reported", progress != null ? `Progress ${progress}%` : "Progress not reported", `Active ${fmtElapsed(now-a.last_activity_at)} ago`, asking ? "Answer needed" : "", a.inbox_unread ? `${a.inbox_unread} unread` : "", a.limit ? "limit" : "", a.compact_pending ? "compact queued" : "", badge(a), a.parent ? `Subagent of ${a.parent}` : ""].filter(Boolean).join(" · "),
  };
}

export function taskSummary(t: Task, tasks: readonly Task[]): EntrySummary {
  return {
    title:t.title,
    info: `${t.assigned_agent ?? "Unassigned"}${t.progress != null ? ` · ${t.progress}% progress` : ""}${t.status === "review" && t.reviewer ? ` · Review: ${t.reviewer}` : ""}`,
    detail:t.dependencies.length ? `Depends on ${t.dependencies.map((id) => `${id} (${tasks.find((x)=>x.id===id)?.status ?? "unresolved"})`).join(", ")}` : "No dependencies",
  };
}
