"use client";

import { Badge, Card, CardContent, Table, TableBody, TableCell, TableHead, TableHeader, TableRow, cn } from "@evinvest/uikit";

import type { ConnectedSite, SourceState } from "@/entities/source";
import { type MessageKey, useLocale, useT } from "@/shared/i18n";
import { formatDateTime } from "@/shared/lib/format";
import { EDGE_CELL, TABLE_HEAD } from "@/shared/ui/table";

const HEAD = cn(TABLE_HEAD, EDGE_CELL);

const STATE_VARIANT = { unset: "neutral", live: "positive", empty: "neutral", withdrawn: "neutral", down: "error" } as const;

/** Every active site key: what its landing says of itself, beside what sites saw of it. */
export function ConnectedSitesTable({ sites }: { sites: ConnectedSite[] }) {
  const t = useT();
  return (
    <Card className="overflow-hidden py-0">
      <CardContent className="overflow-x-auto p-0">
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className={HEAD}>{t("sources.keyId")}</TableHead>
              <TableHead className={HEAD}>{t("sources.brands")}</TableHead>
              <TableHead className={HEAD}>{t("sources.origin")}</TableHead>
              <TableHead className={HEAD}>{t("sources.state")}</TableHead>
              <TableHead className={HEAD}>kitstart</TableHead>
              <TableHead className={cn(HEAD, "text-right")}>{t("connected.lastIngest")}</TableHead>
              <TableHead className={cn(HEAD, "text-right")}>{t("connected.lastRead")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {sites.map((s) => (
              <Row key={s.keyId} site={s} />
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

function Row({ site }: { site: ConnectedSite }) {
  const t = useT();
  const locale = useLocale();
  const seen = (at: string | null | undefined) => (at === undefined ? t("connected.unknown") : at === null ? t("connected.never") : formatDateTime(at, locale));
  const readsTitle = site.reads.map((r) => `${r.brand} ${r.endpoint}: ${formatDateTime(r.at, locale)}`).join("\n");
  return (
    <TableRow>
      <TableCell className={cn(EDGE_CELL, "font-mono text-xs")}>{site.keyId}</TableCell>
      <TableCell className={cn(EDGE_CELL, "text-ink-mid")}>{site.brands.join(", ")}</TableCell>
      <TableCell className={cn(EDGE_CELL, "font-mono text-xs")}>{site.origin ?? "—"}</TableCell>
      <TableCell className={EDGE_CELL}>
        <Report site={site} />
      </TableCell>
      <TableCell className={cn(EDGE_CELL, "tabular-nums text-ink-soft")}>{site.report.state === "reported" ? site.report.report.kitstart : "—"}</TableCell>
      <TableCell className={cn(EDGE_CELL, "text-right tabular-nums text-ink-soft")}>{seen(site.lastIngest)}</TableCell>
      <TableCell className={cn(EDGE_CELL, "text-right tabular-nums text-ink-soft")} title={readsTitle || undefined}>
        {seen(site.lastRead)}
      </TableCell>
    </TableRow>
  );
}

function Report({ site }: { site: ConnectedSite }) {
  const t = useT();
  const { report } = site;
  if (report.state === "no_origin") return <Badge variant="neutral">{t("connected.noOrigin")}</Badge>;
  if (report.state === "failed")
    return (
      <Badge variant="error" title={report.detail}>
        {t("connected.failed")}
      </Badge>
    );
  const { places, pricing, experiments } = report.report.sources;
  return (
    <div className="flex flex-wrap gap-1">
      {Object.entries(places).map(([slug, s]) => (
        <State key={slug} label={slug} state={s} />
      ))}
      <State label={t("connected.pricing")} state={pricing} />
      <State label={t("connected.experiments")} state={experiments} />
    </div>
  );
}

function State({ label, state }: { label: string; state: SourceState }) {
  const t = useT();
  return (
    <Badge variant={STATE_VARIANT[state.state]} title={state.detail ?? undefined}>
      {label}: {t(`connected.state.${state.state}` satisfies MessageKey)}
    </Badge>
  );
}
