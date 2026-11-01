"use client";

import { PageFrame, Settled, Skeleton } from "@evinvest/uikit";
import { useState } from "react";

import { may, useMe } from "@/entities/session";
import { type AddedSource, fetchSources } from "@/entities/source";
import { AddSourceForm, SecretDialog } from "@/features/manage-sources";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";

import { SourcesTable } from "./sources-table";

/** Admin only (spec §5.4): the signing keys the sources ingest with. */
export function SourcesView() {
  const t = useT();
  const me = useMe();
  const sources = useResource("sources", fetchSources, "sources", { live: "sources" });
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
      <SecretDialog added={added} onClose={() => setAdded(null)} />
    </PageFrame>
  );
}
