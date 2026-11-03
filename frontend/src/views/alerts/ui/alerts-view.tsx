"use client";

import {
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Field,
  FieldDescription,
  FieldLabel,
  Input,
  PageFrame,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Skeleton,
  Switch,
} from "@evinvest/uikit";
import { useId, useState } from "react";

import {
  type Alerts,
  type Destination,
  RULES,
  addChat,
  linkTelegram,
  removeDestination,
  setGmail,
  setRules,
  testDestination,
  useAlerts,
} from "@/entities/alerts";
import { type Gmail, fetchGmails } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { ErrorState } from "@/shared/ui/error-state";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

/** Where the caller's alerts go — their own Telegram, channels, groups and topics they run — and which go where. */
export function AlertsView() {
  const t = useT();
  const alerts = useAlerts();
  const gmails = useResource("gmbs", fetchGmails, "gmbs", { focus: true });
  const [busy, setBusy] = useState(false);
  const act = async (write: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await write();
      alerts.reload();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  return (
    <PageFrame breakpoint="md" title={t("alerts.title")} description={t("alerts.description")} width="content">
      {alerts.status === "loading" ? (
        <Skeleton className="h-40 w-full" />
      ) : alerts.status === "error" ? (
        <ErrorState failure={alerts.failure} onRetry={alerts.reload} />
      ) : !alerts.data.enabled ? (
        <p className="text-sm text-ink-soft">{t("alerts.off")}</p>
      ) : (
        <Page alerts={alerts.data} gmails={gmails.status === "ok" ? gmails.data : []} busy={busy} act={act} />
      )}
    </PageFrame>
  );
}

type Act = (write: () => Promise<unknown>) => Promise<void>;

function Page({ alerts, gmails, busy, act }: { alerts: Alerts; gmails: Gmail[]; busy: boolean; act: Act }) {
  const t = useT();
  const own = alerts.destinations.find((d) => d.kind === "private");
  const chats = alerts.destinations.filter((d) => d.kind !== "private");
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
    <>
      <Section title={t("alerts.own")}>
        {own === undefined ? (
          <div className="space-y-1">
            <Button type="button" size="sm" disabled={busy} onClick={connect}>
              {t("alerts.connect")}
            </Button>
            <p className="text-xs text-ink-soft">{t("alerts.connect.hint")}</p>
          </div>
        ) : (
          <DestinationCard destination={own} alerts={alerts} gmails={gmails} busy={busy} act={act} onReconnect={connect} />
        )}
      </Section>
      <Section title={t("alerts.chats")}>
        {chats.map((d) => (
          <DestinationCard key={d.id} destination={d} alerts={alerts} gmails={gmails} busy={busy} act={act} />
        ))}
        {own === undefined ? (
          <p className="text-sm text-ink-soft">{t("alerts.chats.linkFirst")}</p>
        ) : (
          <AddChat bot={alerts.bot} gmails={gmails} busy={busy} act={act} />
        )}
      </Section>
    </>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-ink-soft">{title}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 px-4">{children}</CardContent>
    </Card>
  );
}

function DestinationCard({
  destination: d,
  alerts,
  gmails,
  busy,
  act,
  onReconnect,
}: {
  destination: Destination;
  alerts: Alerts;
  gmails: Gmail[];
  busy: boolean;
  act: Act;
  onReconnect?: () => void;
}) {
  const t = useT();
  const name = d.title ?? t(`alerts.kind.${d.kind}`);
  return (
    <div className="space-y-2 rounded-md border border-border p-3">
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
        <span className="text-ink">
          {name}
          {d.topic_id !== null && <span className="text-ink-soft"> · {t("alerts.topic", { topic: d.topic_id })}</span>}
          {d.kind !== "private" && <span className="text-ink-soft"> · {t(`alerts.kind.${d.kind}`)}</span>}
        </span>
        <span className="flex gap-1">
          <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void act(() => testDestination(d.id))}>
            {t("alerts.test")}
          </Button>
          <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void act(() => removeDestination(d.id))}>
            {t("alerts.remove")}
          </Button>
        </span>
      </div>
      {d.dead !== null && (
        <div className="space-y-1">
          <p className="text-sm text-accent-warn">{t("alerts.dead", { why: d.dead })}</p>
          {onReconnect !== undefined && (
            <Button type="button" size="sm" disabled={busy} onClick={onReconnect}>
              {t("alerts.connect")}
            </Button>
          )}
        </div>
      )}
      {d.kind !== "private" && <p className="text-xs text-ink-soft">{t("alerts.chats.noPii")}</p>}
      {gmails.length > 0 && (
        <GmailScope value={d.gmail_id} gmails={gmails} busy={busy} onChange={(gmail) => void act(() => setGmail(d.id, gmail))} />
      )}
      <Rules alerts={alerts} destination={d} busy={busy} onToggle={(rule, on) => void act(() => setRules(d.id, { [rule]: on }))} />
    </div>
  );
}

const ALL = "all";

function GmailScope({ value, gmails, busy, onChange }: { value: number | null; gmails: Gmail[]; busy: boolean; onChange: (gmail: number | null) => void }) {
  const t = useT();
  const id = useId();
  const size = useControlSize();
  return (
    <Field className="flex flex-col gap-1">
      <FieldLabel htmlFor={id}>{t("alerts.gmail")}</FieldLabel>
      <Select value={value === null ? ALL : String(value)} onValueChange={(v) => onChange(v === ALL ? null : Number(v))}>
        <SelectTrigger id={id} size={size} className="w-full" disabled={busy}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ALL}>{t("alerts.gmail.all")}</SelectItem>
          {gmails.map((g) => (
            <SelectItem key={g.id} value={String(g.id)}>
              {g.gmail}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </Field>
  );
}

function Rules({ alerts, destination, busy, onToggle }: { alerts: Alerts; destination: Destination; busy: boolean; onToggle: (rule: (typeof RULES)[number], on: boolean) => void }) {
  const t = useT();
  if (alerts.rules.length === 0) return <p className="text-sm text-ink-soft">{t("alerts.none")}</p>;
  return (
    <div className="space-y-2 border-t border-border pt-2">
      {RULES.filter((rule) => alerts.rules.includes(rule)).map((rule) => (
        <label key={rule} className="flex items-center justify-between gap-2 text-sm text-ink">
          {t(`alerts.rule.${rule}`)}
          <Switch checked={destination.rules[rule] === true} disabled={busy} onCheckedChange={(next) => onToggle(rule, next)} />
        </label>
      ))}
    </div>
  );
}

function AddChat({ bot, gmails, busy, act }: { bot: string | null; gmails: Gmail[]; busy: boolean; act: Act }) {
  const t = useT();
  const id = useId();
  const size = useControlSize();
  const button = useButtonSize();
  const [chat, setChat] = useState("");
  const [gmail, setGmailScope] = useState<number | null>(null);
  const ready = chat.trim() !== "";
  return (
    <form
      className="grid gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        if (ready) void act(async () => {
          await addChat(chat.trim(), gmail);
          setChat("");
        });
      }}
    >
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={id}>{t("alerts.chats.add")}</FieldLabel>
        <Input id={id} size={size} autoCapitalize="none" value={chat} onChange={(e) => setChat(e.target.value)} placeholder="@channel" />
        <FieldDescription>{t("alerts.chats.hint", { bot: bot ?? "…" })}</FieldDescription>
      </Field>
      {gmails.length > 0 && <GmailScope value={gmail} gmails={gmails} busy={busy} onChange={setGmailScope} />}
      <Button type="submit" disabled={busy || !ready} size={button()}>
        {t("alerts.chats.addButton")}
      </Button>
    </form>
  );
}
