"use client";

import { Badge, Card, CardContent, Table, TableBody, TableCell, TableHead, TableHeader, TableRow, cn } from "@evinvest/uikit";

import { type Source, sourceKindLabel } from "@/entities/source";
import { RevokeButton } from "@/features/manage-sources";
import { useLocale, useT } from "@/shared/i18n";
import { formatDateTime } from "@/shared/lib/format";
import { DESKTOP_QUERY, useMediaQuery } from "@/shared/lib/use-media-query";
import { EDGE_CELL, TABLE_HEAD } from "@/shared/ui/table";

const HEAD = cn(TABLE_HEAD, EDGE_CELL);

/**
 * The keys in one card: a table from `md` up, stacked rows on a phone — a
 * sideways-scrolling table there hides the date and the revoke action, the one
 * thing an admin opens this list for.
 */
export function SourcesTable({ sources, onRevoked }: { sources: Source[]; onRevoked: () => void }) {
  const isDesktop = useMediaQuery(DESKTOP_QUERY);
  return (
    <Card className="overflow-hidden py-0">
      <CardContent className="p-0">{isDesktop ? <Wide sources={sources} onRevoked={onRevoked} /> : <Narrow sources={sources} onRevoked={onRevoked} />}</CardContent>
    </Card>
  );
}

function State({ source, onRevoked }: { source: Source; onRevoked: () => void }) {
  const t = useT();
  const locale = useLocale();
  if (source.revoked_at) return <Badge tone="neutral">{t("sources.revoked", { at: formatDateTime(source.revoked_at, locale) })}</Badge>;
  return <RevokeButton keyId={source.key_id} onRevoked={onRevoked} />;
}

function Wide({ sources, onRevoked }: { sources: Source[]; onRevoked: () => void }) {
  const t = useT();
  const locale = useLocale();
  return (
    <Table>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead className={HEAD}>{t("sources.keyId")}</TableHead>
          <TableHead className={HEAD}>{t("sources.kind")}</TableHead>
          <TableHead className={HEAD}>{t("sources.brands")}</TableHead>
          <TableHead className={cn(HEAD, "text-right")}>{t("sources.created")}</TableHead>
          <TableHead className={HEAD}>
            <span className="sr-only">{t("sources.state")}</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {sources.map((s) => (
          <TableRow key={s.key_id} className={cn(s.revoked_at && "text-ink-soft")}>
            <TableCell className={cn(EDGE_CELL, "font-mono text-xs")}>{s.key_id}</TableCell>
            <TableCell className={EDGE_CELL}>{sourceKindLabel(s.kind, t)}</TableCell>
            <TableCell className={cn(EDGE_CELL, "text-ink-mid")}>{s.brands.join(", ")}</TableCell>
            <TableCell className={cn(EDGE_CELL, "text-right tabular-nums text-ink-soft")}>{formatDateTime(s.created_at, locale)}</TableCell>
            <TableCell className={cn(EDGE_CELL, "text-right")}>
              <State source={s} onRevoked={onRevoked} />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function Narrow({ sources, onRevoked }: { sources: Source[]; onRevoked: () => void }) {
  const t = useT();
  const locale = useLocale();
  return (
    <ul className="divide-y divide-border">
      {sources.map((s) => (
        <li key={s.key_id} className={cn("flex items-start justify-between gap-3 px-4 py-3", s.revoked_at && "text-ink-soft")}>
          <div className="flex min-w-0 flex-col gap-0.5">
            <span className="truncate font-mono text-xs">{s.key_id}</span>
            <span className="text-sm text-ink-mid">
              {sourceKindLabel(s.kind, t)} · {s.brands.join(", ")}
            </span>
            <span className="text-xs tabular-nums text-ink-soft">{formatDateTime(s.created_at, locale)}</span>
          </div>
          <div className="shrink-0">
            <State source={s} onRevoked={onRevoked} />
          </div>
        </li>
      ))}
    </ul>
  );
}
