import type { T } from "@/shared/i18n";

import type { ArchiveTag, Gmb, SiteTag } from "./gmb";

/** What needs the operator on a GMB, most pressing first. */
export const ISSUES = ["overdue", "suspended", "failing", "unlocated", "no_site", "off"] as const;
export type Issue = (typeof ISSUES)[number];

/** The services' tags by GMB id; `null`: the service did not answer, or the caller does not see it. */
export interface Tags {
  archive: ReadonlyMap<number, ArchiveTag> | null;
  sites: ReadonlyMap<number, SiteTag> | null;
}

/** The GMB's most pressing issue; `null` when nothing needs the operator. */
export function issueOf(gmb: Gmb, tags: Tags): Issue | null {
  const archive = tags.archive?.get(gmb.id);
  const site = tags.sites?.get(gmb.id);
  if (site !== undefined && site.overdue > 0) return "overdue";
  if (archive !== undefined && archive.suspended > 0) return "suspended";
  if (archive?.failing) return "failing";
  if (gmb.location === null) return "unlocated";
  if (tags.sites !== null && site === undefined) return "no_site";
  if (!gmb.enabled) return "off";
  return null;
}

/** How many of `gmbs` have each issue as their most pressing one. */
export function issueCounts(gmbs: readonly Gmb[], tags: Tags): Map<Issue, number> {
  const counts = new Map<Issue, number>();
  for (const gmb of gmbs) {
    const issue = issueOf(gmb, tags);
    if (issue !== null) counts.set(issue, (counts.get(issue) ?? 0) + 1);
  }
  return counts;
}

/** The issue as a row and a card name it: with its count where it has one. `issue` is `issueOf` the GMB under these tags. */
export function issueText(issue: Issue, gmb: number, tags: Tags, t: T): string {
  if (issue === "overdue") return t("gmbs.tag.overdue", { n: tags.sites!.get(gmb)!.overdue });
  if (issue === "suspended") return t("gmbs.tag.suspended", { n: tags.archive!.get(gmb)!.suspended });
  return t(`gmbs.issue.${issue}`);
}
