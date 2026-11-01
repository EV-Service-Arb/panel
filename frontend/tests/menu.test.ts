import { describe, expect, it } from "vitest";

import { panelGroups } from "@/features/account-chip/model/attributes";
import { ownScreens, seesSites, startRouteFor } from "@/entities/session/model/access";
import { type Caller, type Permission, ALIASES } from "@/entities/session/model/generated";
import type { T } from "@/shared/i18n";

const t: T = (key) => key;

const holding = (...permissions: Permission[]): Caller => ({ user_id: "u", email: "e", preferred_name: "", permissions, dev_sign_in: false });
const alias = (name: keyof typeof ALIASES): Caller => holding(...ALIASES[name]);

/** What the top bar offers a caller: the Sites dropdown, where `/` lands, the screens beside the chip, the chip's entries past the profile. */
function check(caller: Caller, walletPage: boolean, expected: { sites: boolean; start: string; own: string[]; chip: string[][] }) {
  const chip = panelGroups(caller, t).map((g) => g.items.map((i) => i.id));
  expect({ sites: seesSites(caller), start: startRouteFor(caller), own: ownScreens(caller, walletPage), chip }).toEqual(expected);
}

describe("the top bar, by what the caller holds", () => {
  it("offers someone with nothing the review archive and their own usage, and a chip of their account alone", () => {
    check(holding(), true, { sites: false, start: "/review_archive/", own: ["usage"], chip: [] });
    check(alias("sa:everyone"), true, { sites: false, start: "/review_archive/", own: ["usage"], chip: [] });
  });

  it("lands an operator on the new leads, with the Sites dropdown and the wallet when built", () => {
    check(alias("sa:operator"), true, { sites: true, start: "/sites/leads?stage=created", own: ["usage", "wallet"], chip: [] });
    check(alias("sa:operator"), false, { sites: true, start: "/sites/leads?stage=created", own: ["usage"], chip: [] });
  });

  it("gives Sources to admins alone, and lands them on the leads too", () => {
    check(alias("sa:admin"), true, { sites: true, start: "/sites/leads?stage=created", own: ["sources", "usage", "wallet"], chip: [["act-as"]] });
  });

  it("shows Sites for either section the dropdown lists, and nothing for a sites write alone", () => {
    check(holding("sa:sites:analysis:read"), true, { sites: true, start: "/review_archive/", own: ["usage"], chip: [] });
    check(holding("sa:sites:work:leads:edit", "sa:sites:work:pii:see"), true, { sites: false, start: "/review_archive/", own: ["usage"], chip: [] });
  });
});
