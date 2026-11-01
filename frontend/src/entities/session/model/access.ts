import { ROUTES } from "@/shared/config/routes";

import type { Caller, Permission } from "./generated";

/** Whether the caller holds `permission`: the set concierge resolved is concrete. */
export function may(caller: Caller, permission: Permission): boolean {
  return caller.permissions.includes(permission);
}

/** The sites dropdown lists the Work and Analysis sections: anything else it would show empty. */
export function seesSites(caller: Caller): boolean {
  return may(caller, "sa:sites:work:read") || may(caller, "sa:sites:analysis:read");
}

/** The caller's own screens, beside the account chip, left to right. `walletPage`: the build flag. */
export function ownScreens(caller: Caller, walletPage: boolean): ("sources" | "usage" | "tokens" | "wallet")[] {
  return [
    ...(may(caller, "sa:admin:sources:manage") ? (["sources"] as const) : []),
    "usage" as const,
    "tokens" as const,
    ...(walletPage && may(caller, "sa:sites:work:read") ? (["wallet"] as const) : []),
  ];
}

/** Where a person lands (spec §10): whoever works the sites the queue of new leads, anyone else the review archive, open to all. */
export function startRouteFor(caller: Caller): string {
  return may(caller, "sa:sites:work:read") ? ROUTES.sitesNewLeads : ROUTES.reviewArchive;
}
