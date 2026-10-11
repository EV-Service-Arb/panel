"use client";

import { Button, cn } from "@evinvest/uikit";
import { LogOut } from "lucide-react";

import type { Caller } from "@/entities/session";
import { useT } from "@/shared/i18n";

import { fallbackIdentity } from "../model/attributes";

/**
 * The chip's stand-in while the cabinet bundle loads, and for good when it cannot (not
 * deployed, older, unreachable). Drawn in the chip's box — avatar 34, gap 10, the name — with
 * a sign-out button where the chip has its chevron: whatever happens to the bundle, the way
 * out of the session stays on the page.
 */
export function ChipFallback({ me, onSignOut, className }: { me: Pick<Caller, "preferred_name" | "email">; onSignOut: () => void; className?: string }) {
  const t = useT();
  const { label, initials } = fallbackIdentity(me);
  return (
    // After the name the chip has gap 10 + chevron 16 + padding 6; gap 4 + a 28 button is the
    // same 32, so the cell does not narrow when the chip takes over (that moved the bar's end).
    <div className={cn("flex min-w-0 items-center gap-1 py-1 pl-1.5", className)}>
      <span className="flex min-w-0 items-center gap-2.5">
        <span aria-hidden className="flex size-8.5 shrink-0 items-center justify-center rounded-full bg-primary-ink/15 text-xs font-semibold text-primary-ink">
          {initials}
        </span>
        <span className="max-w-36 truncate text-sm font-semibold text-ink" title={me.email}>
          {label}
        </span>
      </span>
      <Button type="button" variant="ghost" size="xs" icon aria-label={t("nav.signOut")} title={t("nav.signOut")} onClick={onSignOut}>
        <LogOut aria-hidden className="text-ink-soft" />
      </Button>
    </div>
  );
}
