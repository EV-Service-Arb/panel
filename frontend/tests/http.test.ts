import { describe, expect, it, vi } from "vitest";

import { ApiError, createHttp } from "@/shared/api/http";
import { readCsrf } from "@/shared/api/csrf";

type Call = { url: string; init: RequestInit };

function harness(respond: (call: Call) => Response, cookie = "sa_csrf=tok-dev") {
  const calls: Call[] = [];
  const onUnauthenticated = vi.fn();
  const http = createHttp({
    fetch: async (input, init) => {
      const call = { url: String(input), init: init ?? {} };
      calls.push(call);
      return respond(call);
    },
    cookie: () => cookie,
    onUnauthenticated,
  });
  return { http, calls, onUnauthenticated };
}

const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const headerOf = (c: Call | undefined, name: string) => new Headers(c?.init.headers).get(name);
const any = (v: unknown) => v;

describe("the CSRF header", () => {
  it("goes on every write, from the sa_csrf cookie", async () => {
    const { http, calls } = harness(() => json(201, { event_id: "e" }));
    await http.send("POST", "/api/v1/leads/aquafix/L-1/stage", { stage: "won" }, any);
    await http.send("DELETE", "/api/v1/sources/k", undefined, any);
    expect(calls.map((c) => headerOf(c, "x-sa-csrf"))).toEqual(["tok-dev", "tok-dev"]);
    expect(headerOf(calls[0], "content-type")).toBe("application/json");
  });

  it("is not sent on reads", async () => {
    const { http, calls } = harness(() => json(200, {}));
    await http.get("/api/v1/me", any);
    expect(headerOf(calls[0], "x-sa-csrf")).toBeNull();
  });

  it("prefers the __Host- cookie the backend sets behind https", () => {
    expect(readCsrf("sa_csrf=planted; __Host-sa_csrf=real; other=1")).toBe("real");
    expect(readCsrf("sa_csrf=dev")).toBe("dev");
    expect(readCsrf("__Host-sa_csrf=; x=1")).toBeNull();
    expect(readCsrf("")).toBeNull();
  });
});

describe("the init a write may add", () => {
  it("passes keepalive to fetch", async () => {
    const { http, calls } = harness(() => new Response(null, { status: 204 }));
    await http.send("POST", "/auth/logout", undefined, any, undefined, { keepalive: true });
    expect(calls[0]?.init.keepalive).toBe(true);
  });

  it("leaves keepalive out when no init is given", async () => {
    const { http, calls } = harness(() => new Response(null, { status: 204 }));
    await http.send("POST", "/auth/logout", undefined, any);
    expect(calls[0]?.init).not.toHaveProperty("keepalive");
  });

  it("cannot override the method, the headers or the CSRF header", async () => {
    const { http, calls } = harness(() => json(201, {}));
    const hostile: object = { keepalive: true, method: "GET", headers: { "x-sa-csrf": "forged", accept: "text/plain" } };
    await http.send("POST", "/api/v1/leads", { a: 1 }, any, undefined, hostile);
    expect(calls[0]?.init.method).toBe("POST");
    expect(headerOf(calls[0], "x-sa-csrf")).toBe("tok-dev");
    expect(headerOf(calls[0], "accept")).toBe("application/json");
    expect(calls[0]?.init.keepalive).toBe(true);
  });
});

describe("the gate's answers", () => {
  it("401 sends the browser to sign in", async () => {
    const { http, onUnauthenticated } = harness(() => json(401, { error: "sign in" }));
    await expect(http.get("/api/v1/me", any)).rejects.toMatchObject({ failure: { kind: "unauthenticated" } });
    expect(onUnauthenticated).toHaveBeenCalledOnce();
  });

  it("403 without a grant is 'no access', a 403 csrf is told apart", async () => {
    const noAccess = harness(() => json(403, { error: "no access to the panel" }));
    await expect(noAccess.http.get("/api/v1/me", any)).rejects.toMatchObject({ failure: { kind: "no_access" } });
    const csrf = harness(() => json(403, { error: "csrf" }));
    await expect(csrf.http.send("POST", "/api/v1/leads", {}, any)).rejects.toMatchObject({ failure: { kind: "csrf" } });
  });

  it("503 is 'unavailable' and signs nobody out", async () => {
    const { http, onUnauthenticated } = harness(() => json(503, { error: "sign-in is unavailable, try again" }));
    const err = await http.get("/api/v1/me", any).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).failure.kind).toBe("unavailable");
    expect(onUnauthenticated).not.toHaveBeenCalled();
  });
});
