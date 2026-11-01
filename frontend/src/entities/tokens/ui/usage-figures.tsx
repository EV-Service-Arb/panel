"use client";

import { useT } from "@/shared/i18n";

import type { UsageDay } from "../model/tokens";

const total = (days: UsageDay[], key: "charges" | "tokens") => days.reduce((sum, d) => sum + d[key], 0);

/** The last 7 and 30 days' charges and tokens, and a bar a day. */
export function UsageFigures({ days }: { days: UsageDay[] }) {
  const t = useT();
  const peak = Math.max(1, ...days.map((d) => d.tokens));
  return (
    <>
      {[
        { label: t("account.usage.week"), days: days.slice(-7) },
        { label: t("account.usage.month"), days },
      ].map(({ label, days }) => (
        <div key={label} className="grid grid-cols-(--grid-account-row) items-baseline gap-3 text-sm">
          <span className="text-ink-soft">{label}</span>
          <span className="min-w-0 text-ink">
            {t("account.usage.charges", { n: total(days, "charges") })} · {t("account.usage.spent", { n: total(days, "tokens") })}
          </span>
        </div>
      ))}
      <figure className="flex h-16 items-end gap-0.5 pt-2" aria-label={t("account.usage.chart")} role="img">
        {days.map((d) => (
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
