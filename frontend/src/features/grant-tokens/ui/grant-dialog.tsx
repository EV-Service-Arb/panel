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
  FieldLabel,
  Input,
  ToggleGroup,
  ToggleGroupItem,
  toast,
} from "@evinvest/uikit";
import { useId, useState } from "react";

import { type Change, type Holder, changeTokens } from "@/entities/tokens";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

type Mode = "set" | "add" | "grant";

/** set ≥ 0, add ≥ 1, grant ≥ 1 with a note: a grant is owed, for them to claim. */
function changeOf(mode: Mode, n: number, note: string): Change | null {
  if (!Number.isSafeInteger(n)) return null;
  const said = note.trim();
  const noted = said ? { note: said } : {};
  if (mode === "set") return n >= 0 ? { set: n, ...noted } : null;
  if (mode === "add") return n >= 1 ? { add: n, ...noted } : null;
  return n >= 1 && said ? { grant: n, note: said } : null;
}

export function GrantDialog({ holder, onClose, onDone }: { holder: Holder; onClose: () => void; onDone: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const size = useControlSize();
  const id = useId();
  const [mode, setMode] = useState<Mode>("add");
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const change = changeOf(mode, Number(amount), note);

  const submit = async (c: Change) => {
    setBusy(true);
    try {
      const balance = await changeTokens(holder.user_id, c);
      toast.positive(t("tokens.grant.done", { n: balance }));
      onDone();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (change) void submit(change);
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("tokens.grant.title", { who: holder.email })}</DialogTitle>
            <DialogDescription>{t("tokens.count", { n: holder.balance })}</DialogDescription>
          </DialogHeader>
          <ToggleGroup type="single" variant="outline" value={mode} onValueChange={(v) => v && setMode(v as Mode)}>
            {(["set", "add", "grant"] as const).map((m) => (
              <ToggleGroupItem key={m} value={m}>
                {t(`tokens.grant.mode.${m}`)}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
          <Field className="flex flex-col gap-1">
            <FieldLabel htmlFor={`${id}-amount`}>{t("tokens.grant.amount")}</FieldLabel>
            <Input id={`${id}-amount`} type="number" inputMode="numeric" size={size} value={amount} onChange={(e) => setAmount(e.target.value)} />
          </Field>
          <Field className="flex flex-col gap-1">
            <FieldLabel htmlFor={`${id}-note`}>{t(mode === "grant" ? "tokens.grant.note.required" : "tokens.grant.note")}</FieldLabel>
            <Input id={`${id}-note`} size={size} value={note} onChange={(e) => setNote(e.target.value)} />
          </Field>
          <DialogFooter>
            <Button type="submit" size={button()} disabled={busy || change === null}>
              {t("tokens.grant.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
