// Feature negotiation (notes/protocol-features.md §Common records): one
// bounded request on each workspace connection, before `subscribe`. The page
// asks only for the features whose views it implements, since negotiating
// one changes what the connection carries (epics replaces legacy task
// pushes): each view adds its key when it lands. What a daemon accepted holds
// for that connection only; a new connection negotiates again.

import { CallError, type Conn } from "./conn";

/** The features this build's views implement, at the version they speak. */
export const WEB_FEATURES: Readonly<Record<string, number>> = { usage_metrics: 1 };

export type FeatureSession = {
  accepted: Record<string, number>;
  connection_epoch: string;
  workspace_run_id?: string | null;
  /** Requested features the daemon has but cannot serve now, with the reason. */
  unavailable: Record<string, string>;
};

/** `legacy`: the daemon predates negotiation; `unavailable`: it refused or
 * answered with something this page cannot use. */
export type Features =
  | { state: "pending" }
  | { state: "legacy" }
  | { state: "unavailable"; reason: string }
  | { state: "negotiated"; session: FeatureSession };

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === "string";

/** A session that grants only what was asked, at the version asked for. */
export function featureSession(v: unknown): FeatureSession | null {
  if (!isObj(v) || !isObj(v.accepted) || !isObj(v.unavailable)) return null;
  const { accepted, unavailable } = v;
  if (!isStr(v.connection_epoch) || !v.connection_epoch || (v.workspace_run_id != null && !isStr(v.workspace_run_id))) return null;
  for (const k in accepted) if (WEB_FEATURES[k] !== accepted[k]) return null;
  for (const k in unavailable) if (!(k in WEB_FEATURES) || k in accepted || !isStr(unavailable[k])) return null;
  return v as FeatureSession;
}

/** Negotiates on a connection that just proved itself. A timeout or a lost
 * connection throws, and the connection recovers as from any failed call. */
export async function negotiate(conn: Conn): Promise<Features> {
  let reply: unknown;
  try {
    reply = await conn.call("features_negotiate", { versions: WEB_FEATURES });
  } catch (e) {
    if (!(e instanceof CallError) || e.code === "timeout" || e.code === "closed") throw e;
    // A v6 daemon's decoder rejects the method it does not know.
    if (e.code === "invalid" && e.message.includes("unknown variant `features_negotiate`")) return { state: "legacy" };
    return { state: "unavailable", reason: e.code };
  }
  const session = featureSession(reply);
  return session ? { state: "negotiated", session } : { state: "unavailable", reason: "malformed" };
}

/** Whether `name` was accepted at the version this page speaks. */
export const accepts = (f: Features, name: string): boolean => f.state === "negotiated" && name in WEB_FEATURES && f.session.accepted[name] === WEB_FEATURES[name];
