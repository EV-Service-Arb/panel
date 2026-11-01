"use client";

import { Alert, AlertDescription, Badge, Settled, Skeleton, Tabs, TabsContent, TabsList, TabsTrigger } from "@evinvest/uikit";
import { useCallback, useState } from "react";

import { type PlaceKey, type PlaceSettingsView, fetchPlaceSettings, followsPlace, savedSince } from "@/entities/place";
import { may, useMe } from "@/entities/session";
import { SettingsForm, SettingsSummary } from "@/features/edit-place-settings";
import { WithdrawButton } from "@/features/withdraw-place";
import { useLocale, useT } from "@/shared/i18n";
import { formatDateTime } from "@/shared/lib/format";
import { useResource } from "@/shared/lib/use-resource";
import { ErrorState } from "@/shared/ui/error-state";

import { PlaceHistory } from "./place-history";

/**
 * A place's live site data: the form (or, for an operator, what it says), the
 * history with reverts, and for an admin the switch that withdraws the point.
 *
 * The form works from `pinned` — the place as it was when the form started —
 * while the read underneath follows live changes. Every write of one's own
 * pins what it returned; someone else's save shows up as `fresher` and the
 * form decides what to do with it.
 */
export function PlaceSettingsPanel({ place, onChanged }: { place: PlaceKey; onChanged: () => void }) {
  const [version, setVersion] = useState(0);
  const [saved, setSaved] = useState(false);
  const [pinned, setPinned] = useState<PlaceSettingsView | null>(null);
  const id = `${place.brand}/${place.slug}`;
  const settings = useResource(`settings:${id}:${version}`, () => fetchPlaceSettings(place), `settings:${id}`, { live: followsPlace(place) });
  const latest = settings.status === "ok" ? settings.data : null;
  if (latest !== null && pinned === null) setPinned(latest);

  /** After a write: `view` is what the server now holds; without one, read it again. */
  const changed = (note: boolean) => (view?: PlaceSettingsView) => {
    setSaved(note);
    setPinned(view ?? null);
    setVersion((v) => v + 1);
    onChanged();
  };
  const takeFresh = useCallback(() => {
    setSaved(false);
    if (latest !== null) setPinned(latest);
  }, [latest]);

  return (
    <Settled loading={settings.status === "loading"} skeleton={<Skeleton className="h-64 w-full" />}>
      {settings.status === "error" && <ErrorState failure={settings.failure} onRetry={settings.reload} />}
      {latest !== null && (
        <PlacePanes
          base={pinned ?? latest}
          latest={latest}
          version={version}
          saved={saved}
          onWritten={changed(true)}
          onReload={changed(false)}
          onTakeFresh={takeFresh}
        />
      )}
    </Settled>
  );
}

interface PanesProps {
  base: PlaceSettingsView;
  latest: PlaceSettingsView;
  version: number;
  saved: boolean;
  onWritten: (view?: PlaceSettingsView) => void;
  /** A change that needs no "saved" note (a withdrawal), or a 409's re-read. */
  onReload: (view?: PlaceSettingsView) => void;
  onTakeFresh: () => void;
}

function PlacePanes({ base, latest, version, saved, onWritten, onReload, onTakeFresh }: PanesProps) {
  const t = useT();
  const me = useMe();
  const fresher = savedSince(latest, base) ? latest : null;
  return (
    <div className="flex flex-col gap-4">
      <PlaceStatus view={latest} />
      {saved && (
        <Alert variant="success">
          <AlertDescription>{t("placeSettings.saved")}</AlertDescription>
        </Alert>
      )}
      <Tabs defaultValue="edit">
        <TabsList>
          <TabsTrigger value="edit">{t("placeSettings.tab.edit")}</TabsTrigger>
          <TabsTrigger value="history">{t("placeSettings.tab.history")}</TabsTrigger>
        </TabsList>
        <TabsContent value="edit" className="pt-3">
          {base.can_edit ? (
            <SettingsForm key={base.updated_at ?? "never"} place={base} onSaved={onWritten} onReload={() => onReload()} fresher={fresher} onTakeFresh={onTakeFresh} />
          ) : (
            // Nothing typed to protect: a reader sees the newest at once.
            <SettingsSummary place={latest} />
          )}
        </TabsContent>
        <TabsContent value="history" className="pt-3">
          <PlaceHistory view={base} version={version} onChanged={onWritten} onReload={() => onReload()} />
        </TabsContent>
      </Tabs>
      {may(me, "sa:sites:work:places:edit") && (
        // Last and apart: taking a point off the site is rare and not part of editing it.
        <div className="flex flex-col items-start gap-2 border-t border-border pt-4">
          <WithdrawButton place={latest} onChanged={onReload} />
        </div>
      )}
    </div>
  );
}

function PlaceStatus({ view }: { view: PlaceSettingsView }) {
  const t = useT();
  const locale = useLocale();
  return (
    <div className="flex flex-col gap-2 text-sm">
      {view.withdrawn && (
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="destructive">{t("placeSettings.badge.withdrawn")}</Badge>
          <span className="text-ink-mid">{t("placeSettings.withdrawn.body")}</span>
        </div>
      )}
      <p className="text-ink-soft">
        {view.updated_at ? t("placeSettings.updated", { at: formatDateTime(view.updated_at, locale), by: view.updated_by ?? "—" }) : t("placeSettings.never")}
      </p>
    </div>
  );
}
