import { ALIASES, type AccessRequest, PERMISSIONS, type Permission } from "@/entities/session";
import { safeLocalPath } from "@/shared/api";

/** What can be asked for: a permission, or an alias of them. */
export type Need = AccessRequest["need"];

export const NEEDS: readonly Need[] = [...PERMISSIONS, ...(Object.keys(ALIASES) as (keyof typeof ALIASES)[])];

export const PLAYBOOK_NEED: Need = "sa:playbook:mcp:use";

/** The sign-in's `return_to` rule (`panel_server::signin`): a path on this origin, nothing a browser reads as another. */
export const safeContinue = safeLocalPath;

export function holds(permissions: readonly Permission[], need: Need): boolean {
  const members: readonly Permission[] = need in ALIASES ? ALIASES[need as keyof typeof ALIASES] : [need as Permission];
  return members.every((p) => permissions.includes(p));
}
