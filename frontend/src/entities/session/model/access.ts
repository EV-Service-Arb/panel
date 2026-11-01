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

/** Where a person lands (spec §10): whoever works the sites the queue of new leads, anyone else the review archive, open to all. */
export function startRouteFor(caller: Caller): string {
  return may(caller, "sa:sites:work:read") ? ROUTES.sitesNewLeads : ROUTES.reviewArchive;
}
