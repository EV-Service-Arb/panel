import { describe, expect, it } from "vitest";

import { type Invoice, canonicalAmount, expiresInDays, memoProblem, payableInvoiceId } from "@/entities/invoice";
import { type Caller, type Permission } from "@/entities/session/model/generated";
import { type PayOutcome, parsePayMessage, payActionUrl, payNotice, payResultFromEvent, refetchesAfter } from "@/features/pay-invoice-element";
import { invoiceFrom, keepsKey } from "@/features/create-invoice";
import type { ApiFailure } from "@/shared/api";
import type { T } from "@/shared/i18n";
import { panelNav } from "@/views/shell/ui/nav-items";
import type { Marks } from "@/views/shell/model/use-marks";

const t: T = (key) => key;
const marks: Marks = { leads: 0, away: 0, places: 0, experiments: false, sources: false, bookings: false };
const caller = (permissions: readonly Permission[]): Caller => ({ user_id: "u", email: "e", preferred_name: "", permissions: [...permissions], dev_sign_in: false, account_center: null });
const WORK: readonly Permission[] = ["sa:work:read"];
const workItems = (permissions: readonly Permission[], wallet: boolean) => panelNav(t, caller(permissions), marks, { wallet }).groups.find((g) => g.id === "work")?.items.map((i) => i.id);
const also = (permissions: readonly Permission[], wallet: boolean) => panelNav(t, caller(permissions), marks, { wallet }).tabs.find((i) => i.id === "more")?.also ?? [];

describe("the invoices rail entry", () => {
  it("ends the work group and joins the More tab when the caller may work", () => {
    expect(workItems(WORK, false)?.at(-1)).toBe("invoices");
    expect(also(WORK, false)).toContain("/invoices");
  });

  it("is absent without sa:work:read", () => {
    expect(workItems(["sa:analysis:read"], true)).toBeUndefined();
    expect(also(["sa:analysis:read"], true)).not.toContain("/invoices");
  });

  it("follows the wallet entry when that is on", () => {
    expect(workItems(WORK, true)?.slice(-2)).toEqual(["wallet", "invoices"]);
    expect(workItems(WORK, false)).not.toContain("wallet");
  });
});

describe("canonicalAmount", () => {
  it.each([
    ["12.5", "12.5"],
    ["12.50", "12.5"],
    ["0012.050", "12.05"],
    ["1000", "1000"],
    ["1000.", "1000"],
    [".5", "0.5"],
    ["0.000001", "0.000001"],
    [" 7 ", "7"],
    ["999999999.999999", "999999999.999999"],
    ["12,5", "12.5"],
  ])("reads %j as %j, like the backend", (raw, canonical) => {
    expect(canonicalAmount(raw)).toBe(canonical);
  });

  it.each(["", ".", "0", "0.0", "00", "-1", "+1", "1e3", "1.2.3", "0.0000001", "1000000000", "١٢", "1 000", "NaN", "1,2.3"])("refuses %j", (raw) => {
    expect(canonicalAmount(raw)).toBeNull();
  });
});

describe("memoProblem", () => {
  it("counts characters, not bytes: 140 Cyrillic letters fit and 141 do not", () => {
    expect(memoProblem("я".repeat(140))).toBeNull();
    expect(memoProblem("я".repeat(141))).toBe("too_long");
  });

  it("counts an astral character once", () => {
    expect(memoProblem("😀".repeat(140))).toBeNull();
    expect(memoProblem("😀".repeat(141))).toBe("too_long");
  });

  it("refuses a line break", () => {
    expect(memoProblem("a\nb")).toBe("control");
  });
});

describe("expiresInDays", () => {
  it("takes whole days from 1 to 30", () => {
    expect(expiresInDays("1")).toBe(1);
    expect(expiresInDays("30")).toBe(30);
  });

  it.each(["0", "31", "1.5", ""])("refuses %j", (raw) => {
    expect(expiresInDays(raw)).toBeNull();
  });
});

describe("invoiceFrom", () => {
  it("leaves the memo out when it is empty, and sends the amount as a string", () => {
    const out = invoiceFrom({ amount: "12,50", memo: "  ", days: "7" });
    expect(out).toEqual({ ok: true, invoice: { amount: "12.5", expires_in_days: 7 } });
    expect(out.ok && "memo" in out.invoice).toBe(false);
  });

  it("keeps a trimmed memo", () => {
    expect(invoiceFrom({ amount: "1", memo: " за работу ", days: "1" })).toEqual({ ok: true, invoice: { amount: "1", memo: "за работу", expires_in_days: 1 } });
  });

  it("names every problem at once", () => {
    expect(invoiceFrom({ amount: "0", memo: "a\nb", days: "31" })).toEqual({ ok: false, problems: ["amount", "memo_control", "days"] });
    expect(invoiceFrom({ amount: "1", memo: "я".repeat(141), days: "1" })).toEqual({ ok: false, problems: ["memo_too_long"] });
  });
});

describe("keepsKey", () => {
  it("replays the idempotency key after a failure that may have reached banking", () => {
    const keeps: ApiFailure[] = [{ kind: "network" }, { kind: "unavailable" }, { kind: "failed", status: 500, message: "" }];
    expect(keeps.map(keepsKey)).toEqual([true, true, true]);
  });

  it("drops the key after a refusal, which created nothing", () => {
    const drops: ApiFailure[] = [
      { kind: "unauthenticated" },
      { kind: "no_access" },
      { kind: "csrf" },
      { kind: "forbidden", message: "" },
      { kind: "not_found" },
      { kind: "conflict", message: "invoice_not_open", body: null },
      { kind: "bad_request", message: "" },
      { kind: "invalid_fields", fields: {} },
      { kind: "invalid_path", path: "amount", message: "" },
    ];
    expect(drops.map(keepsKey)).toEqual(drops.map(() => false));
  });
});

const row = (over: Partial<Invoice>): Invoice => ({
  id: "i1",
  invoice_id: "inv_123",
  amount: "1",
  currency: "USDT",
  memo: "",
  state: "open",
  pay_url: null,
  created_by: "u",
  created_at: "2026-01-01T00:00:00Z",
  expires_at: "2026-01-08T00:00:00Z",
  paid_at: null,
  paid_by: null,
  ...over,
});

describe("payableInvoiceId", () => {
  it("is the banking id of an open row", () => {
    expect(payableInvoiceId(row({}))).toBe("inv_123");
  });

  it.each(["processing", "paid", "creating", "cancelled", "expired", "unknown"] as const)("is null for a %s row", (state) => {
    expect(payableInvoiceId(row({ state }))).toBeNull();
  });

  it("is null for an open row without a usable id", () => {
    expect(payableInvoiceId(row({ invoice_id: null }))).toBeNull();
    expect(payableInvoiceId(row({ invoice_id: "a/b" }))).toBeNull();
  });
});

describe("payNotice and refetchesAfter", () => {
  const result = (outcome: "done" | "failed" | "cancelled"): PayOutcome => ({ status: "result", result: { outcome, reference: null } });

  it("never says paid: a done popup reads as sent", () => {
    expect(payNotice(result("done"))).toBe("invoices.pay.sent");
    const all: PayOutcome[] = [result("done"), result("failed"), result("cancelled"), { status: "closed" }, { status: "timeout" }, { status: "blocked" }, { status: "busy" }, { status: "invalid_url" }, { status: "unsupported" }];
    expect(all.map((o) => /paid/.test(payNotice(o) ?? ""))).toEqual(all.map(() => false));
  });

  it("reads the row again where the payment may have moved", () => {
    expect(refetchesAfter(result("done"))).toBe(true);
    expect(refetchesAfter({ status: "closed" })).toBe(true);
    expect(refetchesAfter({ status: "timeout" })).toBe(true);
    expect(refetchesAfter({ status: "busy" })).toBe(false);
    expect(refetchesAfter({ status: "blocked" })).toBe(false);
  });
});

describe("parsePayMessage", () => {
  const msg = { type: "ev:wallet-result", state: "s1", outcome: "done", reference: "ref-1" };

  it("reads a message for its own popup", () => {
    expect(parsePayMessage(msg, "s1")).toEqual({ outcome: "done", reference: "ref-1" });
  });

  it("ignores a wrong type, a wrong state and an unknown outcome", () => {
    expect(parsePayMessage({ ...msg, type: "other" }, "s1")).toBeNull();
    expect(parsePayMessage({ ...msg, state: "s2" }, "s1")).toBeNull();
    expect(parsePayMessage({ ...msg, outcome: "paid" }, "s1")).toBeNull();
    expect(parsePayMessage("ev:wallet-result", "s1")).toBeNull();
  });

  it("drops a 129-character reference instead of cutting it, and keeps a 128-character one", () => {
    expect(parsePayMessage({ ...msg, reference: "r".repeat(129) }, "s1")).toEqual({ outcome: "done", reference: null });
    expect(parsePayMessage({ ...msg, reference: "r".repeat(128) }, "s1")?.reference).toBe("r".repeat(128));
  });
});

describe("payResultFromEvent", () => {
  const result = (detail: unknown) => payResultFromEvent(new CustomEvent("ev:wallet-result", { detail }));

  it("reads the in-page form's result for a payment", () => {
    expect(result({ kind: "pay", outcome: "done", reference: "inv_1" })).toEqual({ outcome: "done", reference: "inv_1" });
    expect(result({ kind: "pay", outcome: "cancelled" })).toEqual({ outcome: "cancelled", reference: null });
  });

  it("ignores another kind, an unknown outcome, no detail and a plain event", () => {
    expect(result({ kind: "move", outcome: "done" })).toBeNull();
    expect(result({ kind: "pay", outcome: "paid" })).toBeNull();
    expect(result(null)).toBeNull();
    expect(payResultFromEvent(new Event("ev:wallet-result"))).toBeNull();
  });

  it("drops a reference over 128 characters instead of cutting it", () => {
    expect(result({ kind: "pay", outcome: "done", reference: "r".repeat(129) })).toEqual({ outcome: "done", reference: null });
  });
});

describe("payActionUrl", () => {
  const O = "https://evinvest.ltd";

  it("rebuilds the pay URL: other params and the hash are dropped, the invoice stays", () => {
    const url = payActionUrl(`${O}/wallet/pay?invoice=inv_1&x=1&opener=evil&state=evil#frag`);
    expect(url?.toString()).toBe(`${O}/wallet/pay?invoice=inv_1`);
  });

  it("returns a URL the caller can extend with opener and state", () => {
    const url = payActionUrl(`${O}/wallet/pay?invoice=inv_1`);
    url?.searchParams.set("opener", "https://panel.example");
    url?.searchParams.set("state", "s1");
    expect(url?.searchParams.get("opener")).toBe("https://panel.example");
    expect(url?.searchParams.get("state")).toBe("s1");
  });

  it.each([
    ["another origin", "https://evil.example/wallet/pay?invoice=a"],
    ["another path", `${O}/wallet/withdraw?invoice=a`],
    ["a backslash", `${O}/wallet\\pay?invoice=a`],
    ["a slash in the id", `${O}/wallet/pay?invoice=a/b`],
    ["credentials", "https://u:p@evinvest.ltd/wallet/pay?invoice=a"],
    ["a non-string", 42],
  ])("refuses %s", (_name, raw) => {
    expect(payActionUrl(raw)).toBeNull();
  });
});
