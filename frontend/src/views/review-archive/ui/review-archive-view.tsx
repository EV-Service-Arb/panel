"use client";

import { useSyncExternalStore } from "react";

import { SIGN_IN_PATH, csrfCookieName } from "@/shared/api";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { RemoteElement } from "@/shared/mfe";

const never = () => () => {};

/** review_archive's dashboard, its API and bundle forwarded by the panel (docs/ARCHITECTURE.md, Forward). */
export function ReviewArchiveView() {
  const t = useT();
  const csrf = useSyncExternalStore(never, csrfCookieName, () => null);
  if (csrf === null) return null;
  return (
    <RemoteElement
      className="h-full"
      tag="mfe-review-archive-dashboard"
      scriptUrl="/review_archive/mfe/mfe-review-archive-dashboard.js"
      attributes={{ api: "/api/review_archive", "sign-in": SIGN_IN_PATH, "csrf-cookie": csrf, base: "/review_archive", grafana: ROUTES.grafana, ui: process.env.NEXT_PUBLIC_REVIEW_ARCHIVE_GRAFANA === "0" ? "legacy" : "grafana" }}
      fallback={<p className="p-6 text-muted-foreground">{t("reviewArchive.loading")}</p>}
    />
  );
}
