"use client";

import { Button, Field, FieldDescription, FieldLabel, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, cn } from "@evinvest/uikit";
import { useId, useState } from "react";

import { type AddedSource, KEYED_SOURCE_KINDS, type KeyedSourceKind, addSource, isOrigin, sourceKindLabel } from "@/entities/source";
import { isSlug } from "@/shared/config/brands";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

import { brandsFrom } from "../model/brands";

/** A new signing key: its id, the one kind it writes as, the brands it may write for, and a site's origin. */
export function AddSourceForm({ onAdded }: { onAdded: (added: AddedSource) => void }) {
  const t = useT();
  const button = useButtonSize();
  const id = useId();
  const [keyId, setKeyId] = useState("");
  const [kind, setKind] = useState<KeyedSourceKind>("site");
  const [brandsText, setBrandsText] = useState("");
  const [origin, setOrigin] = useState("");
  const [busy, setBusy] = useState(false);
  const size = useControlSize();
  const brands = brandsFrom(brandsText);
  const site = kind === "site";
  const ready = isSlug(keyId) && brands !== null && (!site || isOrigin(origin));

  const submit = async () => {
    if (!brands) return;
    setBusy(true);
    try {
      onAdded(await addSource({ key_id: keyId, kind, brands, origin: site ? origin : null }));
      setKeyId("");
      setBrandsText("");
      setOrigin("");
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className={cn("grid gap-3 md:items-end", site ? "md:grid-cols-(--grid-site-source-form)" : "md:grid-cols-(--grid-source-form)")}
      onSubmit={(e) => {
        e.preventDefault();
        if (ready) void submit();
      }}
    >
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-key`}>{t("sources.keyId")}</FieldLabel>
        <Input id={`${id}-key`} size={size} autoCapitalize="none" value={keyId} onChange={(e) => setKeyId(e.target.value.toLowerCase())} />
      </Field>
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-kind`}>{t("sources.kind")}</FieldLabel>
        <Select value={kind} onValueChange={(v) => setKind(KEYED_SOURCE_KINDS.find((k) => k === v) ?? "site")}>
          <SelectTrigger id={`${id}-kind`} size={size} className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {KEYED_SOURCE_KINDS.map((k) => (
              <SelectItem key={k} value={k}>
                {sourceKindLabel(k, t)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-brands`}>{t("sources.brands")}</FieldLabel>
        <Input id={`${id}-brands`} size={size} autoCapitalize="none" value={brandsText} onChange={(e) => setBrandsText(e.target.value)} />
        <FieldDescription>{t("sources.brands.hint")}</FieldDescription>
      </Field>
      {site && (
        <Field className="flex flex-col gap-1">
          <FieldLabel htmlFor={`${id}-origin`}>{t("sources.origin")}</FieldLabel>
          <Input id={`${id}-origin`} size={size} autoCapitalize="none" inputMode="url" value={origin} onChange={(e) => setOrigin(e.target.value.trim())} />
          <FieldDescription>{t("sources.origin.hint")}</FieldDescription>
        </Field>
      )}
      <Button type="submit" disabled={busy || !ready} size={button()} className="md:mb-6">
        {t("sources.add")}
      </Button>
    </form>
  );
}
