// The operations (operate.ts) for components, made once per page.
import { createContext, useContext } from "solid-js";
import type { Operations } from "../operate";

export const OpsContext = createContext<Operations>();

export function useOps(): Operations {
  const ops = useContext(OpsContext);
  if (!ops) throw new Error("useOps outside OpsContext");
  return ops;
}
