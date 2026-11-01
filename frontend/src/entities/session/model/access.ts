import { ROUTES } from "@/shared/config/routes";

import type { Caller, Permission } from "./generated";

/** Whether the caller holds `permission`: the set concierge resolved is concrete. */
export function may(caller: Caller, permission: Permission): boolean {
  return caller.permissions.includes(permission);
}

/**
 * Where a person lands (spec §10): whoever manages the sources reads the funnel first;
 * the rest of the team the queue of new leads; anyone else the review archive, open to all.
 */
export function startRouteFor(caller: Caller): string {
  if (may(caller, "sa:admin:sources:manage")) return ROUTES.overview;
  return may(caller, "sa:sites:work:read") ? `${ROUTES.leads}?stage=created` : ROUTES.reviewArchive;
}
