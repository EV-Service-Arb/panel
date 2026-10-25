// Opens the pay page on evinvest.ltd in a popup and waits for its result (banking
// sdk/wallet `openWalletAction`, narrowed to the pay page). Browser only.

import { WALLET_ORIGIN } from "@/shared/config/wallet-bundle";

import { type PayOutcome, parsePayMessage, payActionUrl } from "./action";

const OPENER_PARAM = "opener";
const STATE_PARAM = "state";
const RETURN_TO_PARAM = "return_to";
const TIMEOUT_MS = 15 * 60_000;
const POLL_MS = 500;
const POPUP_FEATURES = "popup=yes,width=480,height=720";
/** The SDK's window name: one action popup per page, whoever opened it. */
const POPUP_NAME = "ev-wallet-action";

// Page-wide: a second click must not reload the first payment's page under it.
let inFlight: Window | null = null;

/** 128 random bits as hex, or null without `crypto`. `getRandomValues` works outside a secure context. */
function freshState(): string | null {
  try {
    return Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
  } catch {
    return null;
  }
}

/**
 * The same payment as a full navigation, for a blocked popup. The page returns to `return_to`
 * only when banking lists the path for scope `sa`; else the payer stays on evinvest.ltd.
 */
export function payNavigationHref(url: URL): string {
  const target = new URL(url);
  target.searchParams.set(RETURN_TO_PARAM, window.location.origin + window.location.pathname);
  return target.href;
}

/**
 * Opens `url` (rebuilt by `payActionUrl`) and resolves how it ended; never rejects. Only a
 * message from this popup, from `WALLET_ORIGIN`, echoing this call's `state` counts. Call it
 * inside the user's click, or the browser blocks the popup. The popup is closed whatever the
 * outcome.
 */
export function openPayPopup(url: URL): Promise<PayOutcome> {
  const target = payActionUrl(url.href);
  if (!target) return Promise.resolve({ status: "invalid_url" });
  if (inFlight && !inFlight.closed) {
    inFlight.focus();
    return Promise.resolve({ status: "busy" });
  }
  const state = freshState();
  if (state === null) return Promise.resolve({ status: "unsupported" });
  target.searchParams.set(OPENER_PARAM, window.location.origin);
  target.searchParams.set(STATE_PARAM, state);
  // Not `noopener`: the pay page needs `window.opener` to post its result back.
  const popup = window.open(target.href, POPUP_NAME, POPUP_FEATURES);
  if (!popup) return Promise.resolve({ status: "blocked" });
  inFlight = popup;

  return new Promise<PayOutcome>((resolve) => {
    let settled = false;
    const settle = (outcome: PayOutcome) => {
      if (settled) return;
      settled = true;
      if (inFlight === popup) inFlight = null;
      window.removeEventListener("message", onMessage);
      clearInterval(poll);
      clearTimeout(timer);
      if (!popup.closed) popup.close();
      resolve(outcome);
    };
    const onMessage = (event: MessageEvent) => {
      // The source check stops another evinvest.ltd window from answering for this popup.
      if (event.origin !== WALLET_ORIGIN || event.source !== popup) return;
      const result = parsePayMessage(event.data, state);
      if (result) settle({ status: "result", result });
    };
    const poll = setInterval(() => {
      if (!popup.closed) return;
      clearInterval(poll);
      // A page that posts and then closes can look closed before its message arrives here;
      // one more macrotask lets the result win.
      setTimeout(() => settle({ status: "closed" }), 0);
    }, POLL_MS);
    const timer = setTimeout(() => settle({ status: "timeout" }), TIMEOUT_MS);
    window.addEventListener("message", onMessage);
  });
}
