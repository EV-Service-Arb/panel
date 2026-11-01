import { describe, expect, it } from "vitest";

import { panelGroups } from "@/features/account-chip/model/attributes";
import { ownScreens, seesSites } from "@/entities/session/model/access";
import { type Caller, type Permission, ALIASES } from "@/entities/session/model/generated";
import type { T } from "@/shared/i18n";

const t: T = (key) => key;

const holding = (...permissions: Permission[]): Caller => ({ user_id: "u", email: "e", preferred_name: "", permissions, dev_sign_in: false });
const alias = (name: keyof typeof ALIASES): Caller => holding(...ALIASES[name]);

/** What the top bar offers a caller: Sites, the screens beside the chip, the chip's entries past the profile. */
function check(caller: Caller, walletPage: boolean, expected: { sites: boolean; own: string[]; chip: string[][] }) {
  const chip = panelGroups(caller, t).map((g) => g.items.map((i) => i.id));
  expect({ sites: seesSites(caller), own: ownScreens(caller, walletPage), chip }).toEqual(expected);
}

describe("the top bar, by what the caller holds", () => {
  it("offers someone with nothing the review archive and their own usage, and a chip of their account alone", () => {
    check(holding(), true, { sites: false, own: ["usage", "tokens"], chip: [] });
    check(alias("sa:everyone"), true, { sites: false, own: ["usage", "tokens"], chip: [] });
  });

  it("gives an operator Sites, and the wallet when built", () => {
    check(alias("sa:operator"), true, { sites: true, own: ["usage", "tokens", "wallet"], chip: [] });
    check(alias("sa:operator"), false, { sites: true, own: ["usage", "tokens"], chip: [] });
  });

  it("gives Sources to admins alone", () => {
    check(alias("sa:admin"), true, { sites: true, own: ["sources", "usage", "tokens", "wallet"], chip: [["act-as"]] });
  });

  it("shows Sites for either section its nav lists, and nothing for a sites write alone", () => {
    check(holding("sa:sites:analysis:read"), true, { sites: true, own: ["usage", "tokens"], chip: [] });
    check(holding("sa:sites:work:leads:edit", "sa:sites:work:pii:see"), true, { sites: false, own: ["usage", "tokens"], chip: [] });
  });
});
