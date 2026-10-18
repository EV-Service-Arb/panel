import { afterEach, describe, expect, it, vi } from "vitest";

import { openOverlay, safeLocalPath, takeReturnPage } from "@/shared/api/signin-overlay";

function stubWindow(pathname: string, search = "", store = new Map<string, string>()) {
  vi.stubGlobal("window", {
    location: { pathname, search },
    sessionStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
  });
  return store;
}

afterEach(() => vi.unstubAllGlobals());

describe("safeLocalPath", () => {
  it.each(["/", "/leads/", "/leads?stage=created", "/places/abc/?tab=1"])("keeps %s", (p) => {
    expect(safeLocalPath(p)).toBe(p);
  });

  it.each([null, "", "leads", "//evil.example", "/\\evil.example", "\\\\evil", "javascript:alert(1)", "https://evil.example/", "/a b", "/a\nb", `/${"a".repeat(600)}`])(
    "drops %j",
    (p) => {
      expect(safeLocalPath(p)).toBeNull();
    },
  );
});

describe("the page kept across the sign-in dialog", () => {
  it("is remembered on open and given back once", () => {
    stubWindow("/leads/", "?stage=created");
    openOverlay();
    expect(takeReturnPage()).toBe("/leads/?stage=created");
    expect(takeReturnPage()).toBeNull();
  });

  it("is not used when the stored value is not a safe path", () => {
    const store = stubWindow("/leads/");
    store.set("sa:return-after-signin", "//evil.example");
    expect(takeReturnPage()).toBeNull();
    expect(store.size).toBe(0);
  });

  it("is null when storage throws", () => {
    vi.stubGlobal("window", {
      location: { pathname: "/leads/", search: "" },
      sessionStorage: {
        getItem: () => {
          throw new Error("blocked");
        },
        setItem: () => {
          throw new Error("blocked");
        },
        removeItem: () => undefined,
      },
    });
    expect(() => openOverlay()).not.toThrow();
    expect(takeReturnPage()).toBeNull();
  });
});
