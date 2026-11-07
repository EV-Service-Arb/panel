import type { Gmail, Gmb } from "./gmb";

export interface Listed {
  gmb: Gmb;
  gmail: Gmail;
}

/** The GMBs under `scope` (`null`: every gmail) whose name, address, place id or gmail holds `filter`. */
export function narrowed(gmails: readonly Gmail[], scope: number | null, filter: string): Listed[] {
  const needle = filter.trim().toLowerCase();
  return gmails
    .filter((g) => scope === null || g.id === scope)
    .flatMap((gmail) => gmail.gmbs.map((gmb) => ({ gmb, gmail })))
    .filter(({ gmb, gmail }) => needle === "" || [gmb.name, gmb.address, gmb.place_id, gmail.gmail].some((s) => s?.toLowerCase().includes(needle)));
}

/** In the viewport on, in it off, and elsewhere: those without a point too. */
export function byView(listed: readonly Listed[], inView: ReadonlySet<number>): { inView: Listed[]; offInView: Listed[]; elsewhere: Listed[] } {
  return {
    inView: listed.filter((l) => inView.has(l.gmb.id) && l.gmb.enabled),
    offInView: listed.filter((l) => inView.has(l.gmb.id) && !l.gmb.enabled),
    elsewhere: listed.filter((l) => !inView.has(l.gmb.id)),
  };
}
