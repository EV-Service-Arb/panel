"use client";

import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Field, FieldLabel, Input } from "@evinvest/uikit";
import { Plus } from "lucide-react";
import { useId, useState } from "react";

import { addGmail } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

/** The backend lowercases and trims; an address or an alias for one, without spaces. */
const looksLikeEmail = (v: string) => /^\S+$/.test(v.trim());

export function AddGmailButton({ onAdded }: { onAdded: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const size = useControlSize();
  const id = useId();
  const [open, setOpen] = useState(false);
  const [gmail, setGmail] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setBusy(true);
    try {
      await addGmail(gmail.trim());
      setOpen(false);
      setGmail("");
      onAdded();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Button size={button()} onClick={() => setOpen(true)}>
        <Plus aria-hidden />
        {t("gmbs.gmail.add")}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              if (looksLikeEmail(gmail)) void submit();
            }}
          >
            <DialogHeader>
              <DialogTitle>{t("gmbs.gmail.add")}</DialogTitle>
              <DialogDescription>{t("gmbs.gmail.hint")}</DialogDescription>
            </DialogHeader>
            <Field className="flex flex-col gap-1">
              <FieldLabel htmlFor={`${id}-gmail`}>{t("gmbs.gmail.label")}</FieldLabel>
              <Input id={`${id}-gmail`} size={size} autoCapitalize="none" value={gmail} onChange={(e) => setGmail(e.target.value)} />
            </Field>
            <DialogFooter>
              <Button type="submit" size={button()} disabled={busy || !looksLikeEmail(gmail)}>
                {t("gmbs.gmail.add")}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  );
}
