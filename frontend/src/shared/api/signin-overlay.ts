// State of the sign-in overlay, outside React: a 401 is seen by `http` and the live socket, which
// are not components. The overlay is opt-in at build time (`NEXT_PUBLIC_SIGNIN_OVERLAY=1`); off, or
// with no host component mounted to show it, `requestSignInOverlay` says no and the caller
// redirects as it always did.

export type OverlayState = "idle" | "open" | "dismissed";

/** Inlined at build time by Next; anything but "1" keeps the full redirect. */
export const SIGNIN_OVERLAY_ENABLED = process.env.NEXT_PUBLIC_SIGNIN_OVERLAY === "1";

let state: OverlayState = "idle";
let hosts = 0;
const listeners = new Set<() => void>();

function set(next: OverlayState): void {
  if (state === next) return;
  state = next;
  for (const l of listeners) l();
}

export function subscribeOverlay(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function overlayState(): OverlayState {
  return state;
}

/** A component that can show the overlay is mounted; without one a request must not be swallowed. */
export function registerOverlayHost(): () => void {
  hosts += 1;
  return () => {
    hosts -= 1;
  };
}

/** True when the overlay took the 401; false means the caller must redirect itself. */
export function requestSignInOverlay(): boolean {
  if (!SIGNIN_OVERLAY_ENABLED || hosts === 0) return false;
  rememberPage();
  set("open");
  return true;
}

export function openOverlay(): void {
  rememberPage();
  set("open");
}

export function dismissOverlay(): void {
  set("dismissed");
}

// The dialog's OAuth round trip may only come back to a path the tenant registered (`/auth/login`),
// not to the page the person was on, so the page is kept here and put back after the callback.
const RETURN_KEY = "sa:return-after-signin";
const MAX_PATH = 512;

/** A path on this origin, nothing a browser reads as another (`//host`, `/\host`, `javascript:`); else null. */
export function safeLocalPath(raw: string | null): string | null {
  if (raw === null) return null;
  const ok = raw.length <= MAX_PATH && raw.startsWith("/") && raw[1] !== "/" && !raw.includes("\\") && /^[\x21-\x7e]+$/.test(raw);
  return ok ? raw : null;
}

function rememberPage(): void {
  if (typeof window === "undefined") return;
  try {
    const here = safeLocalPath(window.location.pathname + window.location.search);
    if (here !== null) window.sessionStorage.setItem(RETURN_KEY, here);
  } catch {
    // Storage blocked: the person lands on the start page, as before.
  }
}

/** The page to go back to after signing in, once: the key is cleared on read. Null: none, or not a safe path. */
export function takeReturnPage(): string | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.sessionStorage.getItem(RETURN_KEY);
    window.sessionStorage.removeItem(RETURN_KEY);
    return safeLocalPath(raw);
  } catch {
    return null;
  }
}
