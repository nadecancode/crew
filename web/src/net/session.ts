// The page's credential: the one module that touches `localStorage`
// (notes/web-design.md §security).
//
// - A pairing code arrives in the URL fragment (`#pair=<code>&w=<id>`) and is
//   taken out of the address before anything else runs; what is left is the
//   address of the workspace `w` names (`#/w/<id>/`), the one the page
//   opens first (notes/web-multi.md §multi-ui "Addresses").
// - The session (token and verifier) is kept under the one key
//   `crew.web.session`. It is read once, when the document starts; from then
//   on only the copy in this module's memory is trusted. There is no
//   `storage` listener: what another document of the origin writes later
//   changes nothing here.
// - The proof the server shows in `hello` is hashed and compared with the
//   verifier, and then forgotten; it is never stored.
// - Nothing here is logged, put in a URL, or kept in a store a component can
//   render.

import { WORKSPACE_ID } from "../route";

export const STORAGE_KEY = "crew.web.session";

export type Credential = { readonly token: string; readonly verifier: string };

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;
type Place = { hash: string; pathname: string; search: string };
type History = { replaceState(data: unknown, unused: string, url?: string): void };

/** Take a pairing code out of the address: `#pair=<code>&w=<id>` is replaced
 * by `#/w/<id>/` (or nothing) before any connection is made, and before the
 * router reads the fragment. */
export function takePairCode(place: Place, history: History): string | null {
  // Read before the address changes: with the real `location` the hash is gone after.
  const hash = place.hash;
  if (!hash.startsWith("#pair=")) return null;
  const [code, ...rest] = hash.slice("#pair=".length).split("&");
  const w = new URLSearchParams(rest.join("&")).get("w");
  history.replaceState(null, "", place.pathname + place.search + (w && WORKSPACE_ID.test(w) ? `#/w/${w}/` : ""));
  return /^[A-Za-z0-9_-]{1,256}$/.test(code) ? code : null;
}

function parse(raw: string | null): Credential | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    if (v && typeof v.token === "string" && typeof v.verifier === "string" && v.token && /^[0-9a-f]{64}$/.test(v.verifier)) {
      return { token: v.token, verifier: v.verifier };
    }
  } catch {
    // Not ours, or damaged: as if there were none.
  }
  return null;
}

/** The session of this document. Made once, at the document's start. */
export class Session {
  private credential: Credential | null;
  /** A pairing code from the address, traded on the first connection. */
  private code: string | null;

  constructor(private readonly store: Store | null, code: string | null) {
    let raw: string | null = null;
    try {
      raw = store?.getItem(STORAGE_KEY) ?? null;
    } catch {
      // Storage refused (a private window, blocked site data): no session kept.
    }
    this.credential = parse(raw);
    this.code = code;
  }

  /** The pairing code still to be traded; null once traded or when there was none. */
  takeCode(): string | null {
    const code = this.code;
    this.code = null;
    return code;
  }

  pending(): boolean {
    return this.code !== null;
  }

  current(): Credential | null {
    return this.credential;
  }

  /** A pairing gave this document a new session: it replaces the old one,
   * in memory and in storage. */
  adopt(credential: Credential): void {
    this.credential = credential;
    try {
      this.store?.setItem(STORAGE_KEY, JSON.stringify({ token: credential.token, verifier: credential.verifier }));
    } catch {
      // Kept in memory for this document only.
    }
  }

  /** The page stopped (shutdown, a failed proof, unauthorized): the session
   * is removed from storage and forgotten. */
  remove(): void {
    this.credential = null;
    this.code = null;
    try {
      this.store?.removeItem(STORAGE_KEY);
    } catch {
      // Nothing to remove.
    }
  }
}

const hex = (bytes: ArrayBuffer) => Array.from(new Uint8Array(bytes), (b) => b.toString(16).padStart(2, "0")).join("");

/** Is `proof` the secret whose SHA-256 is this session's verifier? */
export async function proves(proof: unknown, credential: Credential): Promise<boolean> {
  if (typeof proof !== "string" || proof.length === 0) return false;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(proof));
  return hex(digest) === credential.verifier;
}
