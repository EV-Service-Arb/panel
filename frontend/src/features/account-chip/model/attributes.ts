import { type Caller, may } from "@/entities/session";
import { switchAccountPath } from "@/shared/api";
import { ROUTES } from "@/shared/config/routes";
import type { T } from "@/shared/i18n";

// The host side of the account chip's host-mode contract (apiVersion 2). The panel has
// its own origin and session, so the chip fetches nothing and shows what these attributes say.

/** The first bundle with `data-mode="host"`; older ones read a cabinet session the panel lacks. */
export const CHIP_API_VERSION = 2;

/** Every attribute the panel may set, so one it stops sending is removed rather than left stale. */
export const CHIP_ATTRIBUTE_NAMES = ["data-mode", "data-name", "data-email", "data-profile-href", "data-switch-href", "data-groups"] as const;

export type ChipAttributeName = (typeof CHIP_ATTRIBUTE_NAMES)[number];
export type ChipAttributes = Partial<Record<ChipAttributeName, string>>;

export interface ChipItem {
  id: string;
  href: string;
  label: string;
  external?: boolean;
  current?: boolean;
}

export interface ChipGroup {
  id: string;
  label?: string;
  items: ChipItem[];
}

// The chip drops a name or email longer than this outright; cutting it keeps the name shown.
const MAX_TEXT = 254;

function clip(value: string): string {
  const chars = Array.from(value.trim());
  return chars.length > MAX_TEXT ? chars.slice(0, MAX_TEXT).join("") : chars.join("");
}

/** The panel's own menu entries, the ones the kit's AccountMenu showed before. */
export function panelGroups(me: Caller, t: T): ChipGroup[] {
  const items: ChipItem[] = [];
  if (may(me, "sa:review_archive:members:act_as")) {
    items.push({ id: "act-as", href: ROUTES.reviewArchiveActAs, label: t("account.actAs"), external: true });
  }
  items.push({ id: "tokens", href: ROUTES.reviewArchiveTokens, label: t("account.tokens.history"), external: true });
  return [{ id: "sa", items }];
}

/** The element's attributes for `me` on `pathname`; an absent value is an absent attribute. */
export function chipAttributes(me: Caller, pathname: string, t: T): ChipAttributes {
  const attrs: ChipAttributes = {
    "data-mode": "host",
    "data-profile-href": ROUTES.account,
    "data-switch-href": switchAccountPath(pathname),
    "data-groups": JSON.stringify(panelGroups(me, t)),
  };
  const name = clip(me.preferred_name);
  if (name) attrs["data-name"] = name;
  // Not clipped: a cut address is someone else's; one over 254 is not valid and the chip drops it.
  const email = me.email.trim();
  if (email) attrs["data-email"] = email;
  return attrs;
}

/** What the fallback shows: the name, else the email; its initials the way the kit's avatar takes them. */
export function fallbackIdentity(me: Pick<Caller, "preferred_name" | "email">): { label: string; initials: string } {
  const name = me.preferred_name.trim();
  const words = name.split(/\s+/).filter(Boolean);
  const letters = words.length > 0 ? words.slice(0, 2).map((w) => Array.from(w)[0] ?? "") : [Array.from(me.email.trim())[0] ?? ""];
  return { label: name || me.email, initials: letters.join("").toUpperCase() };
}
