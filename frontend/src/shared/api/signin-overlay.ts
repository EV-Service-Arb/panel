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
  set("open");
  return true;
}

export function openOverlay(): void {
  set("open");
}

export function dismissOverlay(): void {
  set("dismissed");
}
