"use client";

import { Card, CardContent, CardHeader, CardTitle, Skeleton } from "@evinvest/uikit";

import { type Permission, useMe } from "@/entities/session";
import { type Usage, fetchTokens, fetchUsage } from "@/entities/tokens";
import { ROUTES } from "@/shared/config/routes";
import { type MessageKey, useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { ScreenFrame } from "@/shared/ui/screen-frame";

/** The rail's sections, by a permission's second segment. */
const SECTION: Record<string, MessageKey> = {
  work: "nav.group.work",
  analysis: "nav.group.analysis",
  admin: "nav.group.admin",
  review_archive: "nav.group.archive",
  playbook: "account.access.playbook",
};

function sectionOf(p: Permission): MessageKey {
  const key = SECTION[p.split(":")[1] ?? ""];
  if (key === undefined) throw new Error(`no rail section for ${p}`);
  return key;
}

/** Service-Arb's page about the signed-in account: who they are is evinvest.ltd's account page, the rest is this service's. */
export function AccountView() {
  const t = useT();
  return (
    <ScreenFrame title={t("account.title")} description={t("account.description")} back={ROUTES.more} width="content">
      <Access />
      <TokensCard />
      <UsageCard />
      <Section title={t("account.integrations")}>
        <a className="text-sm text-ink hover:underline" href={ROUTES.reviewArchiveTelegram}>
          {t("account.telegram")}
        </a>
        <p className="text-sm text-ink-soft">{t("account.telegram.hint")}</p>
      </Section>
    </ScreenFrame>
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

function TokensCard() {
  const t = useT();
  const tokens = useResource("account.tokens", fetchTokens);
  return (
    <Section title={t("account.tokens")}>
      {tokens.status === "loading" && <Skeleton className="h-10 w-48" />}
      {tokens.status === "error" && <p className="text-sm text-ink-soft">{t("account.tokens.unavailable")}</p>}
      {tokens.status === "ok" && (
        <>
          <Row label={t("account.tokens.now")}>
            <span className="text-lg font-semibold tabular-nums">{t("tokens.count", { n: tokens.data.balance })}</span>
          </Row>
          <p className="text-sm text-ink-soft">{t("account.tokens.renewal", { daily: tokens.data.daily, cap: tokens.data.cap })}</p>
        </>
      )}
      <a className="text-sm text-ink hover:underline" href={ROUTES.reviewArchiveTokens}>
        {t("account.tokens.history")}
      </a>
    </Section>
  );
}

const total = (days: Usage["days"], key: "walks" | "tokens") => days.reduce((sum, d) => sum + d[key], 0);

function UsageCard() {
  const t = useT();
  const usage = useResource("account.usage", fetchUsage);
  return (
    <Section title={t("account.usage")}>
      {usage.status === "loading" && <Skeleton className="h-24 w-full" />}
      {usage.status === "error" && <p className="text-sm text-ink-soft">{t("account.usage.unavailable")}</p>}
      {usage.status === "ok" && <UsageFigures usage={usage.data} />}
    </Section>
  );
}

function UsageFigures({ usage }: { usage: Usage }) {
  const t = useT();
  const week = usage.days.slice(-7);
  const peak = Math.max(1, ...usage.days.map((d) => d.tokens));
  return (
    <>
      {[
        { label: t("account.usage.week"), days: week },
        { label: t("account.usage.month"), days: usage.days },
      ].map(({ label, days }) => (
        <Row key={label} label={label}>
          {t("account.usage.walks", { n: total(days, "walks") })} · {t("account.usage.spent", { n: total(days, "tokens") })}
        </Row>
      ))}
      <Row label={t("account.usage.places")}>
        <span className="tabular-nums">{usage.places_tracked}</span>
      </Row>
      <figure className="flex h-16 items-end gap-0.5 pt-2" aria-label={t("account.usage.chart")} role="img">
        {usage.days.map((d) => (
          <span
            key={d.day}
            title={`${d.day}: ${t("account.usage.spent", { n: d.tokens })}`}
            className="flex-1 rounded-t-sm bg-primary/70"
            style={{ height: `${Math.max(2, (d.tokens / peak) * 100)}%` }}
          />
        ))}
      </figure>
    </>
  );
}
