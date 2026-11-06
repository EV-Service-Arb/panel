"use client";

import { type ReactNode, createContext, useContext } from "react";

import { type Resource, useResource } from "@/shared/lib/use-resource";

import { fetchGmails } from "../api/gmbs";
import { fetchArchive, fetchSiteTags } from "../api/tags";
import type { Archive, Gmail, SiteTag } from "./gmb";
import type { Tags } from "./issues";

type Read<T> = Resource<T> & { reload: () => void };

interface Gmbs {
  gmails: Read<Gmail[]>;
  archive: Read<Archive | null>;
  sites: Read<Map<number, SiteTag> | null>;
}

const GmbsContext = createContext<Gmbs | null>(null);

/** One read each of the caller's gmails and the services' tags on their GMBs, for the outline and the page beside it: a change made in one is seen by the other. */
export function GmailsProvider({ seesSites, children }: { seesSites: boolean; children: ReactNode }) {
  const gmails = useResource("gmbs", fetchGmails, "gmbs", { focus: true });
  const archive = useResource("gmbs.archive", fetchArchive, "gmbs.archive", { focus: true });
  const sites = useResource(`gmbs.sites.${seesSites}`, () => (seesSites ? fetchSiteTags() : Promise.resolve(null)), "gmbs.sites", { focus: true });
  return <GmbsContext value={{ gmails, archive, sites }}>{children}</GmbsContext>;
}

function useGmbs(): Gmbs {
  const gmbs = useContext(GmbsContext);
  if (gmbs === null) throw new Error("used outside GmailsProvider");
  return gmbs;
}

export function useGmails(): Read<Gmail[]> {
  return useGmbs().gmails;
}

/** The services' reads, and their tags as an issue is judged by: one not answered yet has none. */
export function useGmbTags(): { archive: Gmbs["archive"]; sites: Gmbs["sites"]; tags: Tags } {
  const { archive, sites } = useGmbs();
  return {
    archive,
    sites,
    tags: {
      archive: archive.status === "ok" ? (archive.data?.gmbs ?? null) : null,
      sites: sites.status === "ok" ? sites.data : null,
    },
  };
}
