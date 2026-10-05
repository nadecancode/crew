import type { Task } from "../protocol";

export type EntrySummary = { name?: string; status?: string; tone?: string; title: string; info: string; detail: string };

export function taskSummary(t: Task, tasks: readonly Task[]): EntrySummary {
  return {
    title:t.title,
    info: `${t.assigned_agent ?? "Unassigned"}${t.progress != null ? ` · ${t.progress}% progress` : ""}${t.status === "review" && t.reviewer ? ` · Review: ${t.reviewer}` : ""}`,
    detail:t.dependencies.length ? `Depends on ${t.dependencies.map((id) => `${id} (${tasks.find((x)=>x.id===id)?.status ?? "unresolved"})`).join(", ")}` : "No dependencies",
  };
}
