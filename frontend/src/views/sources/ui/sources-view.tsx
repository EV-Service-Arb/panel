"use client";

import { PageFrame, Settled, Skeleton } from "@evinvest/uikit";
import { useState } from "react";

import { may, useMe } from "@/entities/session";
import { type AddedSource, fetchConnectedSites, fetchSources } from "@/entities/source";
import { AddSourceForm, SecretDialog } from "@/features/manage-sources";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";

import { ConnectedSitesTable } from "./connected-sites-table";
import { SourcesTable } from "./sources-table";

/** Admin only (spec §5.4): the signing keys the sources ingest with. */
export function SourcesView() {
  const t = useT();
  const me = useMe();
  const sources = useResource("sources", fetchSources, "sources", { live: "sources" });
  const connected = useResource("connected-sites", fetchConnectedSites, "connected-sites", { live: "sources" });
  const [added, setAdded] = useState<AddedSource | null>(null);

  if (!may(me, "sa:admin:sources:manage")) {
    return (
      <PageFrame breakpoint="md" title={t("sources.title")}>
        <ErrorState failure={{ kind: "forbidden", message: "" }} />
      </PageFrame>
    );
  }

  return (
    <PageFrame breakpoint="md" title={t("sources.title")}>
      <AddSourceForm
        onAdded={(a) => {
          setAdded(a);
          sources.reload();
        }}
      />
      <Settled loading={sources.status === "loading"} skeleton={<Skeleton className="h-40 w-full" />}>
        {sources.status === "error" && <ErrorState failure={sources.failure} onRetry={sources.reload} />}
        {sources.status === "ok" && sources.data.length === 0 && <EmptyState title={t("sources.empty")} description={t("sources.empty.body")} />}
        {sources.status === "ok" && sources.data.length > 0 && (
          <SourcesTable sources={sources.data} onRevoked={sources.reload} />
        )}
      </Settled>
      <h2 className="text-lg font-semibold">{t("connected.title")}</h2>
      <Settled loading={connected.status === "loading"} skeleton={<Skeleton className="h-24 w-full" />}>
        {connected.status === "error" && <ErrorState failure={connected.failure} onRetry={connected.reload} />}
        {connected.status === "ok" &&
          connected.data.sitesFailures.map((detail) => (
            <p key={detail} className="text-sm text-ink-soft">
              {t("connected.sitesFailed", { detail })}
            </p>
          ))}
        {connected.status === "ok" && connected.data.sites.length === 0 && <EmptyState title={t("connected.empty")} description={t("connected.empty.body")} />}
        {connected.status === "ok" && connected.data.sites.length > 0 && <ConnectedSitesTable sites={connected.data.sites} />}
      </Settled>
      <SecretDialog added={added} onClose={() => setAdded(null)} />
    </PageFrame>
  );
}
