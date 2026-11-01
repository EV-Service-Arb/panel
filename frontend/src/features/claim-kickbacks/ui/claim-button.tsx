"use client";

import { Button, toast } from "@evinvest/uikit";
import { useState } from "react";

import { claimKickbacks } from "@/entities/tokens";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize } from "@/shared/ui/touch";

export function ClaimButton({ total, onClaimed }: { total: number; onClaimed: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const [busy, setBusy] = useState(false);
  const claim = async () => {
    setBusy(true);
    try {
      await claimKickbacks();
      toast.positive(t("tokens.claimed", { n: total }));
      onClaimed();
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button size={button("sm")} disabled={busy} onClick={() => void claim()}>
      {t("tokens.claim")}
    </Button>
  );
}
