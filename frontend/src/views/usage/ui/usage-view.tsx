"use client";

import { Card, CardContent, CardHeader, CardTitle, PageFrame, Skeleton } from "@evinvest/uikit";

import { type Permission, useMe } from "@/entities/session";
import { fetchArchive } from "@/entities/gmb";
import { UsageFigures, fetchUsage } from "@/entities/tokens";
import { ROUTES } from "@/shared/config/routes";
import { type MessageKey, useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";

/** Who grants it, by a permission's second segment: the service, or the conductor's own admin. */
const SECTION: Record<string, MessageKey> = {
  sites: "nav.sites",
  admin: "usage.access.admin",
  review_archive: "nav.reviewArchive",
  playbook: "account.access.playbook",
};

function sectionOf(p: Permission): MessageKey {
  const key = SECTION[p.split(":")[1] ?? ""];
  if (key === undefined) throw new Error(`no section for ${p}`);
  return key;
}

/** What the signed-in account may do and has spent on Service-Arb; who they are is the Profile screen. */
export function UsageView() {
  const t = useT();
  return (
    <PageFrame breakpoint="md" title={t("usage.title")} description={t("usage.description")} width="content">
      <Access />
      <UsageCard />
      <Section title={t("account.integrations")}>
        <a className="text-sm text-ink hover:underline" href={ROUTES.alerts}>
          {t("account.alerts")}
        </a>
        <p className="text-sm text-ink-soft">{t("account.alerts.hint")}</p>
      </Section>
    </PageFrame>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-ink-soft">{title}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-2 px-4">{children}</CardContent>
    </Card>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-(--grid-account-row) items-baseline gap-3 text-sm">
      <span className="text-ink-soft">{label}</span>
      <span className="min-w-0 text-ink">{children}</span>
    </div>
  );
}

function Access() {
  const t = useT();
  const me = useMe();
  const sections = new Map<MessageKey, Permission[]>();
  for (const p of me.permissions) sections.set(sectionOf(p), [...(sections.get(sectionOf(p)) ?? []), p]);
  return (
    <Section title={t("account.access")}>
      {sections.size === 0 && <p className="text-sm text-ink-soft">{t("account.access.none")}</p>}
      {[...sections].map(([section, permissions]) => (
        <Row key={section} label={t(section)}>
          <span className="flex flex-wrap gap-x-3 gap-y-1">
            {permissions.map((p) => (
              <code key={p} className="text-xs">
                {p}
              </code>
            ))}
          </span>
        </Row>
      ))}
    </Section>
  );
}

function UsageCard() {
  const t = useT();
  const usage = useResource("account.usage", fetchUsage);
  const archive = useResource("account.archive", fetchArchive);
  return (
    <Section title={t("account.usage")}>
      {usage.status === "loading" && <Skeleton className="h-24 w-full" />}
      {usage.status === "error" && <p className="text-sm text-ink-soft">{t("account.usage.unavailable")}</p>}
      {usage.status === "ok" && <UsageFigures days={usage.data} />}
      <Row label={t("account.usage.places")}>
        <span className="tabular-nums">{archive.status === "ok" && archive.data !== null ? archive.data.places_tracked : "—"}</span>
      </Row>
    </Section>
  );
}
