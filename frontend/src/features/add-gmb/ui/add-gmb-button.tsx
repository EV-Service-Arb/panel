"use client";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Field,
  FieldDescription,
  FieldLabel,
  Input,
  Label,
  RadioGroup,
  RadioGroupItem,
} from "@evinvest/uikit";
import { Plus } from "lucide-react";
import { useRouter } from "next/navigation";
import { useId, useState } from "react";

import { type Candidate, addGmb, resolvePlace, setOnMap } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

/** A place or Maps URL pasted, resolved to candidates, one picked and added under `gmailId`; one Google has no point for is then placed on the home map. */
export function AddGmbButton({ gmailId, onAdded }: { gmailId: number; onAdded: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const size = useControlSize();
  const id = useId();
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [place, setPlace] = useState("");
  const [candidates, setCandidates] = useState<Candidate[] | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reset = () => {
    setPlace("");
    setCandidates(null);
    setPicked(null);
  };

  const act = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  const find = () =>
    act(async () => {
      const found = await resolvePlace(place.trim());
      setCandidates(found);
      setPicked(found.length === 1 ? found[0]!.place_id : null);
    });

  const add = (placeId: string) =>
    act(async () => {
      const added = await addGmb(gmailId, placeId);
      setOpen(false);
      reset();
      onAdded();
      if (added.location === null) {
        router.push("/");
        setOnMap({ placing: added.id, peeked: null });
      }
    });

  return (
    <>
      <Button variant="ghost" size={button("sm")} icon aria-label={t("gmbs.gmb.add")} title={t("gmbs.gmb.add")} onClick={() => setOpen(true)}>
        <Plus aria-hidden />
      </Button>
      <Dialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) reset();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("gmbs.gmb.add")}</DialogTitle>
            <DialogDescription>{t("gmbs.gmb.place.hint")}</DialogDescription>
          </DialogHeader>
          <form
            className="flex items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (place.trim()) void find();
            }}
          >
            <Field className="flex min-w-0 flex-1 flex-col gap-1">
              <FieldLabel htmlFor={`${id}-place`}>{t("gmbs.gmb.place")}</FieldLabel>
              <Input
                id={`${id}-place`}
                size={size}
                autoCapitalize="none"
                value={place}
                onChange={(e) => {
                  setPlace(e.target.value);
                  setCandidates(null);
                  setPicked(null);
                }}
              />
            </Field>
            <Button type="submit" variant="outline" size={button()} disabled={busy || !place.trim()}>
              {t("gmbs.gmb.find")}
            </Button>
          </form>
          {candidates !== null && candidates.length === 0 && <FieldDescription>{t("gmbs.gmb.none")}</FieldDescription>}
          {candidates !== null && candidates.length > 0 && (
            <RadioGroup value={picked ?? ""} onValueChange={setPicked} aria-label={t("gmbs.gmb.pick")} className="flex flex-col gap-2">
              {candidates.map((c) => (
                <div key={c.place_id} className="flex items-start gap-3 rounded-md border border-border p-3">
                  <RadioGroupItem id={`${id}-${c.place_id}`} value={c.place_id} className="mt-0.5" />
                  <Label htmlFor={`${id}-${c.place_id}`} className="flex min-w-0 flex-col items-start gap-0.5 font-normal">
                    <span className="text-sm text-ink">{c.name ?? c.place_id}</span>
                    {c.address && <span className="text-xs text-ink-soft">{c.address}</span>}
                  </Label>
                </div>
              ))}
            </RadioGroup>
          )}
          <DialogFooter>
            <Button size={button()} disabled={busy || picked === null} onClick={() => picked !== null && void add(picked)}>
              {t("gmbs.gmb.add")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
