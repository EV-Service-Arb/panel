// The host side of banking's money-action contract, for the pay button only (banking
// sdk/wallet/src/action.ts at 45aa81c7). It is a port, not the package: `@evinvest/wallet`
// is not a dependency of the panel. Keep the checks in step with the SDK.
//
// Pure: no `window` here, so the checks run in a test without a DOM.

import { WALLET_ORIGIN } from "@/shared/config/wallet-bundle";

/** Cancelable, from the element: the user pressed Pay. `detail` is `{ href }`. */
export const WALLET_ACTION_EVENT = "ev:wallet-action";
/** `type` of the message that the evinvest.ltd page posts to its opener. */
export const WALLET_RESULT_MESSAGE = "ev:wallet-result";

/** The only action the pay button offers. The SDK knows four more; this host opens none of them. */
const PAY_PATH = "/wallet/pay";
const ACTION_PARAMS = ["invoice", "scope"] as const;
// Refused, not cleaned: an id with a separator or an encoded character can make the action page
// look up something other than what the element named.
const ACTION_PARAM_VALUE = /^[A-Za-z0-9_-]{1,128}$/;
const MAX_URL = 2048;
const MAX_REFERENCE = 128;

/**
 * `raw` rebuilt from scratch as the pay page on `origin`, or null. Only the exact path survives,
 * with the first `invoice` and `scope`; every other parameter and the hash are dropped. Another
 * origin, credentials, control characters, backslashes (browsers read `\` as `/`) and a bad id
 * are refused.
 */
export function payActionUrl(raw: unknown, origin: string = WALLET_ORIGIN): URL | null {
  if (typeof raw !== "string" || raw.length === 0 || raw.length > MAX_URL) return null;
  if (/[\u0000-\u001F\u007F\\]/.test(raw)) return null;
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    return null;
  }
  if (parsed.origin !== origin || parsed.username !== "" || parsed.password !== "" || parsed.pathname !== PAY_PATH) return null;
  const url = new URL(PAY_PATH, origin);
  for (const name of ACTION_PARAMS) {
    const value = parsed.searchParams.get(name);
    if (value === null) continue;
    if (!ACTION_PARAM_VALUE.test(value)) return null;
    url.searchParams.set(name, value);
  }
  return url;
}

/** The action URL of a `ev:wallet-action` event, or null when its detail is missing or not one. */
export function payActionFromEvent(event: Event, origin: string = WALLET_ORIGIN): URL | null {
  const detail: unknown = event instanceof CustomEvent ? event.detail : undefined;
  if (typeof detail !== "object" || detail === null) return null;
  return payActionUrl(Reflect.get(detail, "href"), origin);
}

/**
 * What the pay page reported. `done` is a hint for the UI, not proof of payment: the message
 * comes from a window that this page cannot inspect. Paid is what the panel's row says.
 */
export interface PayResult {
  outcome: "done" | "failed" | "cancelled";
  reference: string | null;
}

const OUTCOMES: readonly PayResult["outcome"][] = ["done", "failed", "cancelled"];

/**
 * A `ev:wallet-result` payload for the popup opened with `state`, or null for anything else
 * on the channel. An unusable reference reads as none; it is never cut, because a cut
 * reference names another payment.
 */
export function parsePayMessage(data: unknown, state: string): PayResult | null {
  if (typeof data !== "object" || data === null) return null;
  if (Reflect.get(data, "type") !== WALLET_RESULT_MESSAGE) return null;
  if (state === "" || Reflect.get(data, "state") !== state) return null;
  const outcome: unknown = Reflect.get(data, "outcome");
  const reference: unknown = Reflect.get(data, "reference");
  const known = OUTCOMES.find((value) => value === outcome);
  if (known === undefined) return null;
  const usable = typeof reference === "string" && reference !== "" && reference.length <= MAX_REFERENCE;
  return { outcome: known, reference: usable ? reference : null };
}

/** How a pay popup ended. After `closed` or `timeout` the payment may still have gone through. */
export type PayOutcome =
  | { status: "result"; result: PayResult }
  | { status: "closed" }
  /** The popup was refused; the page navigates to the pay page instead. */
  | { status: "blocked" }
  /** Another pay popup is open on this page; it was focused, and this click reports nothing. */
  | { status: "busy" }
  | { status: "timeout" }
  | { status: "invalid_url" }
  /** No `crypto` to draw a state from; nothing was opened. */
  | { status: "unsupported" };

/**
 * Whether the row is read again from the backend after `outcome`. Every outcome where the
 * payment may have moved is: `done`, but also `closed` and `timeout`. The read, not the
 * outcome, says whether the invoice is paid.
 */
export function refetchesAfter(outcome: PayOutcome): boolean {
  switch (outcome.status) {
    case "result":
    case "closed":
    case "timeout":
      return true;
    case "blocked":
    case "busy":
    case "invalid_url":
    case "unsupported":
      return false;
  }
}

/** The toast for `outcome`, as a message key, or null when nothing needs to be said. Never "paid". */
export function payNotice(outcome: PayOutcome): "invoices.pay.sent" | "invoices.pay.failed" | "invoices.pay.timeout" | "invoices.pay.invalid" | "invoices.pay.unsupported" | null {
  switch (outcome.status) {
    case "result":
      if (outcome.result.outcome === "done") return "invoices.pay.sent";
      return outcome.result.outcome === "failed" ? "invoices.pay.failed" : null;
    case "timeout":
      return "invoices.pay.timeout";
    case "invalid_url":
      return "invoices.pay.invalid";
    case "unsupported":
      return "invoices.pay.unsupported";
    case "closed":
    case "blocked":
    case "busy":
      return null;
  }
}
