"use client";

import { type ReactNode, createContext, useContext } from "react";

import { type Resource, useResource } from "@/shared/lib/use-resource";

import { fetchGmails } from "../api/gmbs";
import type { Gmail } from "./gmb";

type Gmails = Resource<Gmail[]> & { reload: () => void };

const GmailsContext = createContext<Gmails | null>(null);

/** One read of the caller's gmails for the outline and the page beside it: a change made in one is seen by the other. */
export function GmailsProvider({ children }: { children: ReactNode }) {
  const gmails = useResource("gmbs", fetchGmails, "gmbs", { focus: true });
  return <GmailsContext value={gmails}>{children}</GmailsContext>;
}

export function useGmails(): Gmails {
  const gmails = useContext(GmailsContext);
  if (gmails === null) throw new Error("useGmails outside GmailsProvider");
  return gmails;
}
