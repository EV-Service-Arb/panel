import { describe, expect, it } from "vitest";

import { type Caller, type Permission, ALIASES } from "@/entities/session/model/generated";
import type { T } from "@/shared/i18n";
import { panelNav } from "@/views/shell/ui/nav-items";
import type { Marks } from "@/views/shell/model/use-marks";

const t: T = (key) => key;
const marks: Marks = { leads: 0, away: 0, places: 0, experiments: false, sources: false, bookings: false };
const caller = (permissions: readonly Permission[]): Caller => ({ user_id: "u", email: "e", preferred_name: "", permissions: [...permissions], dev_sign_in: false });
const ALIAS = { operator: ALIASES["sa:operator"], admin: ALIASES["sa:admin"] };
const ids = (permissions: readonly Permission[]) => Object.fromEntries(panelNav(t, caller(permissions), marks).groups.map((g) => [g.id, g.items.map((i) => i.id)]));

describe("the rail", () => {
  it("files Grafana under Analysis, as an external link, for every alias", () => {
    for (const permissions of [ALIAS.operator, ALIAS.admin]) {
      expect(ids(permissions).analysis).toEqual(["experiments", "grafana"]);
      const grafana = panelNav(t, caller(permissions), marks).groups.flatMap((g) => g.items).find((i) => i.id === "grafana");
      expect(grafana?.external).toBe(true);
      expect(grafana?.trailing).toBeDefined();
    }
  });

  it("has no Account group: the profile block is the rail's footer, not a nav group", () => {
    expect(Object.keys(ids(ALIAS.admin))).toEqual(["work", "analysis", "admin", "archive"]);
    expect(Object.keys(ids(ALIAS.operator))).toEqual(["work", "analysis", "archive"]);
  });

  it("shows a section only to whoever its permission opens, and the review archive to everyone", () => {
    expect(Object.keys(ids([]))).toEqual(["archive"]);
    expect(Object.keys(ids(["sa:analysis:read"]))).toEqual(["analysis", "archive"]);
    expect(panelNav(t, caller([]), marks).tabs.map((i) => i.id)).toEqual(["review_archive", "more"]);
  });
});
