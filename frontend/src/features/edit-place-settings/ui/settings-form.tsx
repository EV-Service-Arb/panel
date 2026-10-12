"use client";

import { Alert, AlertDescription, Button } from "@evinvest/uikit";
import { useEffect, useState } from "react";

import { ChannelPreview, ConflictAlert, type PlaceSettingsView, StaleAlert } from "@/entities/place";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

import { type SettingsDraft, draftChanged, draftOf, draftValid, editedOf, settingsOf } from "../model/draft";
import { useSave } from "../model/use-save";
import { AreaChips } from "./area-chips";
import { BookingFields } from "./booking-fields";
import { HoursEditor } from "./hours-editor";
import { KeptFields } from "./kept-fields";
import { MessengerSwitches, TelegramField } from "./messenger-fields";
import { PhoneField } from "./phone-field";
import { ReviewUrlField } from "./review-url-field";

export interface SettingsFormProps {
  place: PlaceSettingsView;
  onSaved: (view: PlaceSettingsView) => void;
  /** After a 409: read the place again, dropping this form's edits. */
  onReload: () => void;
  /** The place as saved by someone else since this form was read; null while nobody has. */
  fresher: PlaceSettingsView | null;
  /** Start the form again from `fresher`. */
  onTakeFresh: () => void;
}

/**
 * The editor of a place's live data. Re-mount it (key on `updated_at`) when the
 * place changes, so the draft starts from what was saved.
 *
 * Someone else's save arriving live never replaces what is typed here: with no
 * edits the form simply takes it, with edits it says so and waits for the person.
 */
export function SettingsForm({ place, onSaved, onReload, fresher, onTakeFresh }: SettingsFormProps) {
  const t = useT();
  const button = useButtonSize();
  const base = place.settings;
  const [draft, setDraft] = useState<SettingsDraft>(() => draftOf(base.edited));
  const { state, errors, save } = useSave(place, place.updated_at, onSaved);
  const set = (patch: Partial<SettingsDraft>) => setDraft((d) => ({ ...d, ...patch }));
  const changed = draftChanged(draft, base);
  useEffect(() => {
    if (fresher && !changed) onTakeFresh();
  }, [fresher, changed, onTakeFresh]);

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (changed && draftValid(draft)) void save(settingsOf(draft, base));
      }}
    >
      {state.kind === "conflict" && <ConflictAlert onReload={onReload} />}
      {fresher && changed && state.kind !== "conflict" && <StaleAlert fresher={fresher} onLoad={onTakeFresh} />}
      {state.kind === "invalid" && (
        <Alert variant="destructive">
          <AlertDescription>
            {t("placeSettings.invalid")}
            {errors.other.map((o) => (
              <span key={o.key} className="block">{`${o.key}: ${o.reason}`}</span>
            ))}
          </AlertDescription>
        </Alert>
      )}
      <PhoneField label={t("placeSettings.field.phone")} value={draft.phone} onChange={(phone) => set({ phone })} errors={errors.byField.phone} />
      <PhoneField label={t("placeSettings.field.whatsapp")} value={draft.whatsapp} onChange={(whatsapp) => set({ whatsapp })} errors={errors.byField.whatsapp} />
      <TelegramField value={draft.telegram} onChange={(telegram) => set({ telegram })} errors={errors.byField.telegram} />
      <MessengerSwitches value={draft.messengers} onChange={(messengers) => set({ messengers })} errors={errors.byField.messengers} />
      <HoursEditor rows={draft.hours} onChange={(hours) => set({ hours })} errors={errors.byField.hours} />
      <AreaChips names={draft.serviceArea} onChange={(serviceArea) => set({ serviceArea })} errors={errors.byField.serviceArea} />
      <BookingFields draft={draft.booking} onChange={(booking) => set({ booking })} serverErrors={errors.byKey} />
      <ReviewUrlField value={draft.reviewUrl} onChange={(reviewUrl) => set({ reviewUrl })} errors={errors.byField.reviewUrl} />
      <KeptFields rest={base.rest} />
      <ChannelPreview fields={editedOf(draft)} />
      <div className="flex flex-wrap gap-2">
        <Button type="submit" size={button()} disabled={!changed || !draftValid(draft) || state.kind === "saving" || state.kind === "conflict" || fresher !== null}>
          {t("placeSettings.save")}
        </Button>
        <Button type="button" variant="ghost" size={button()} disabled={!changed || state.kind === "saving"} onClick={() => setDraft(draftOf(base.edited))}>
          {t("placeSettings.reset")}
        </Button>
      </div>
    </form>
  );
}
