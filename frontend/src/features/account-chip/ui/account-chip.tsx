"use client";

import { cn } from "@evinvest/uikit";
import { usePathname } from "next/navigation";

import type { Caller } from "@/entities/session";
import { useLocale, useT } from "@/shared/i18n";
import { useElementBundle } from "@/shared/mfe";

import { CHIP_API_VERSION, CHIP_SCRIPT, CHIP_TAG, chipAttributes } from "../model/attributes";
import { ChipFallback } from "./chip-fallback";
import { useChipElement } from "./use-chip-element";

// A stable object: the loader's effect is keyed on its fields.
const BUNDLE = { minApiVersion: CHIP_API_VERSION, timeoutMs: 8000 } as const;

/**
 * The EV account chip — the one the site and the cabinet show — in host mode: the panel tells
 * it who is signed in and owns Sign out (`signout` → `onSignOut`).
 *
 * The element and the fallback share one grid cell, right-aligned, inside the chip's reserve
 * (178×42): the fallback paints at once and stays on top until the element has painted itself,
 * then leaves. The element is invisible until then, so neither a double chip nor a gap shows;
 * the cell keeps its width when one replaces the other, give or take the name's length.
 */
export function AccountChip({ me, onSignOut, className }: { me: Caller; onSignOut: () => void; className?: string }) {
  const t = useT();
  const locale = useLocale();
  const pathname = usePathname();
  const bundle = useElementBundle(CHIP_TAG, CHIP_SCRIPT, BUNDLE);
  const { hostRef, painted } = useChipElement(bundle === "ready", chipAttributes(me, pathname, t), onSignOut, locale);
  return (
    <div className={cn("grid min-h-10.5 min-w-44.5 items-center justify-items-end", className)}>
      <div ref={hostRef} className="col-start-1 row-start-1 flex items-center" />
      {(bundle !== "ready" || !painted) && <ChipFallback className="col-start-1 row-start-1" me={me} onSignOut={onSignOut} />}
    </div>
  );
}
