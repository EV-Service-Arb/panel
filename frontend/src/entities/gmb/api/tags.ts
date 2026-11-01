import { ApiError, createHttp } from "@/shared/api";

import { type Archive, type SiteTag, archiveParser, sitesParser } from "../model/gmb";

/** A service's 401 is not the panel's session ending: the panel's own reads see to that. */
const forward = createHttp({
  fetch: (input, init) => fetch(input, init),
  cookie: () => (typeof document === "undefined" ? "" : document.cookie),
  onUnauthenticated: () => {},
});

/** A service down or refusing the caller is `null`: its tags are absent and the page stands. A body that fails its check still throws. */
async function absentOnRefusal<T>(read: Promise<T>): Promise<T | null> {
  try {
    return await read;
  } catch (e) {
    if (e instanceof ApiError) return null;
    throw e;
  }
}

export function fetchArchive(): Promise<Archive | null> {
  return absentOnRefusal(forward.get("/api/review_archive/me/gmbs", archiveParser));
}

export function fetchSiteTags(): Promise<Map<number, SiteTag> | null> {
  return absentOnRefusal(forward.get("/api/sites/gmbs", sitesParser));
}
