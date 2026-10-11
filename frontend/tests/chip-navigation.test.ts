import { describe, expect, it } from "vitest";

import { panelRouteFor } from "@/features/account-chip";

// The cabinet's account chip offers its menu links to the panel as `ev:navigate` with
// `detail.href` (path + query + hash). The panel routes a link itself only when it is one of its
// own screens, spelled as its `<Link>`s spell them (`ROUTES`); everything else — the review
// archive's own views, Grafana, auth, the API, the cabinet, any other origin, anything that is
// not a path-absolute string — stays a full navigation by the browser.
describe("panelRouteFor: the panel's own screens", () => {
  it.each([
    ["/account", "/account"],
    ["/account/", "/account"],
    ["/account?x=1#h", "/account?x=1#h"],
    ["/", "/"],
    ["/leads/?stage=new", "/leads?stage=new"],
    ["/review_archive", "/review_archive/"],
    ["/review_archive/", "/review_archive/"],
  ])("routes %s as %s", (href, route) => {
    expect(panelRouteFor(href)).toBe(route);
  });

  it("routes every screen of the rail, query kept", () => {
    expect(["/overview", "/places", "/pricing", "/experiments", "/sources", "/more"].map((p) => panelRouteFor(`${p}?q=1`))).toEqual([
      "/overview?q=1",
      "/places?q=1",
      "/pricing?q=1",
      "/experiments?q=1",
      "/sources?q=1",
      "/more?q=1",
    ]);
  });
});

describe("panelRouteFor: left to the browser", () => {
  it.each([
    ["the archive's tokens view", "/review_archive/tokens"],
    ["the archive's act-as view", "/review_archive/act-as"],
    ["Grafana behind the backend proxy", "/grafana/"],
    ["the account chooser", "/auth/login?prompt=select_account"],
    ["the API", "/api/v1/me"],
    ["the cabinet zone", "/cabinet"],
    ["a protocol-relative URL", "//evil.com/account"],
    ["a backslash the URL parser reads as a slash", "/\\evil.com"],
    ["a tab the URL parser drops between the slashes", "/\t/evil.com/account"],
    ["an absolute URL", "https://x/account"],
    ["a relative path", "account"],
    ["a look-alike screen", "/accounts"],
    ["a path under a screen", "/account/x"],
    ["the signed-out page", "/signed-out"],
  ])("refuses %s (%s)", (_, href) => {
    expect(panelRouteFor(href)).toBeNull();
  });

  // Each guard on its own: these resolve onto the parser's placeholder host, so only the
  // path-shape checks refuse them, whatever the origin check would say.
  it.each([
    ["protocol-relative", "//panel.invalid/account"],
    ["backslash", "/\\panel.invalid/account"],
  ])("refuses a %s href even onto the resolver's own host", (_, href) => {
    expect(panelRouteFor(href)).toBeNull();
  });

  it.each([
    ["undefined", undefined],
    ["a number", 42],
    ["null", null],
    ["an object", { href: "/account" }],
  ])("refuses a href that is %s", (_, href) => {
    expect(panelRouteFor(href)).toBeNull();
  });
});
