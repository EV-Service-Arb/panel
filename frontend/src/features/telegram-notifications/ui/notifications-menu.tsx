"use client";

import { Button, NavDot, Popover, PopoverContent, PopoverTrigger, Skeleton, Switch } from "@evinvest/uikit";
import { Bell } from "lucide-react";
import { useState } from "react";

import { RULES, type Rule, linkTelegram, setRules, unlinkTelegram, useTelegram } from "@/entities/telegram";
import { useT } from "@/shared/i18n";
import { ErrorState } from "@/shared/ui/error-state";
import { notifyFailure } from "@/shared/ui/notify";

/** The bell: where the caller's notifications go (Telegram) and which they get. */
export function NotificationsMenu() {
  const t = useT();
  const telegram = useTelegram();
  const [busy, setBusy] = useState(false);
  const unreached = telegram.status === "ok" && telegram.data.enabled && (!telegram.data.linked || telegram.data.blocked);

  const act = async (write: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await write();
      telegram.reload();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };
  // Opened within the click, or a popup blocker takes it: the link only exists after the POST.
  const connect = () => {
    const tab = window.open("about:blank", "_blank");
    void act(async () => {
      try {
        const url = await linkTelegram();
        if (tab === null) window.location.assign(url);
        else tab.location.href = url;
      } catch (e) {
        tab?.close();
        throw e;
      }
    });
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" variant="ghost" size="sm" icon className="relative" aria-label={t("nav.notifications")}>
          <Bell aria-hidden />
          {unreached && <NavDot corner tone="warn" label={t("telegram.connect")} />}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 space-y-3 p-4">
        <h2 className="text-sm font-semibold text-ink">{t("telegram.title")}</h2>
        {telegram.status === "loading" ? (
          <Skeleton className="h-24 w-full" />
        ) : telegram.status === "error" ? (
          <ErrorState failure={telegram.failure} onRetry={telegram.reload} />
        ) : !telegram.data.enabled ? (
          <p className="text-sm text-ink-soft">{t("telegram.off")}</p>
        ) : (
          <>
            {telegram.data.linked ? (
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-2 text-sm">
                  <span className="text-ink">
                    {t("telegram.linked")}
                    {telegram.data.account !== null && <span className="text-ink-soft"> · {telegram.data.account}</span>}
                  </span>
                  <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void act(unlinkTelegram)}>
                    {t("telegram.disconnect")}
                  </Button>
                </div>
                {telegram.data.blocked && <p className="text-sm text-accent-warn">{t("telegram.blocked")}</p>}
              </div>
            ) : null}
            {(!telegram.data.linked || telegram.data.blocked) && (
              <div className="space-y-1">
                <Button type="button" size="sm" disabled={busy} onClick={connect}>
                  {t("telegram.connect")}
                </Button>
                <p className="text-xs text-ink-soft">{t("telegram.connect.hint")}</p>
              </div>
            )}
            <Rules rules={telegram.data.rules} busy={busy} onToggle={(rule, on) => void act(() => setRules({ [rule]: on }))} />
          </>
        )}
      </PopoverContent>
    </Popover>
  );
}

function Rules({ rules, busy, onToggle }: { rules: Partial<Record<Rule, boolean>>; busy: boolean; onToggle: (rule: Rule, on: boolean) => void }) {
  const t = useT();
  const open = RULES.flatMap((rule) => {
    const on = rules[rule];
    return on === undefined ? [] : [[rule, on] as const];
  });
  if (open.length === 0) return <p className="text-sm text-ink-soft">{t("telegram.none")}</p>;
  return (
    <section className="space-y-2 border-t border-border pt-3">
      <h3 className="text-xs font-medium uppercase tracking-wide text-ink-soft">{t("telegram.rules")}</h3>
      {open.map(([rule, on]) => (
        <label key={rule} className="flex items-center justify-between gap-2 text-sm text-ink">
          {t(`telegram.rule.${rule}`)}
          <Switch checked={on} disabled={busy} onCheckedChange={(next) => onToggle(rule, next)} />
        </label>
      ))}
    </section>
  );
}
