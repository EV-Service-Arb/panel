"use client";

import { cn } from "@evinvest/uikit";
import { ACCOUNT_CHIP_TAG } from "@evinvest/concierge";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useState } from "react";

import type { Caller } from "@/entities/session";
import { platformMfe } from "@/shared/api";
import { useLocale, useT } from "@/shared/i18n";
import { useElementBundle } from "@/shared/mfe";

import { CHIP_API_VERSION, chipAttributes } from "../model/attributes";
import { ChipFallback } from "./chip-fallback";
import { useChipElement } from "./use-chip-element";

// A stable object: the loader's effect is keyed on its fields.
const BUNDLE = { minApiVersion: CHIP_API_VERSION, integrity: null, timeoutMs: 8000 } as const;

/**
 * The EV account chip — the one the site and the cabinet show — in host mode: the panel tells
 * it who is signed in, owns Sign out (`signout` → `onSignOut`) and routes its links to panel
 * screens (`ev:navigate` → `router.push`).
 *
 * The element and the fallback share one grid cell, right-aligned, inside the chip's reserve
 * (178×42): the fallback paints at once and stays on top until the element has painted itself,
 * then leaves. The element is invisible until then, so neither a double chip nor a gap shows;
 * the cell keeps its width when one replaces the other, give or take the name's length.
 */
export function AccountChip({ me, onSignOut, className }: { me: Caller; onSignOut: () => void; className?: string }) {
  const scriptUrl = useChipScriptUrl();
  const cell = cn("grid min-h-10.5 min-w-44.5 items-center justify-items-end", className);
  // Registry not answered yet, or it gave no chip: the fallback alone, the way a bundle that
  // failed to load looks.
  if (scriptUrl === null) {
    return (
      <div className={cell}>
        <ChipFallback className="col-start-1 row-start-1" me={me} onSignOut={onSignOut} />
      </div>
    );
  }
  return <LoadedChip scriptUrl={scriptUrl} me={me} onSignOut={onSignOut} className={cell} />;
}

/** The concierge chip's bundle URL, as the registry names it; null until it answers, and for good if it does not. */
function useChipScriptUrl(): string | null {
  const [scriptUrl, setScriptUrl] = useState<string | null>(null);
  useEffect(() => {
    let stale = false;
    void platformMfe.scriptUrl("accountChip", BUNDLE.timeoutMs).then(url => {
      if (!stale && url !== null) setScriptUrl(url);
    });
    return () => {
      stale = true;
    };
  }, []);
  return scriptUrl;
}

interface LoadedChipProps {
  scriptUrl: string;
  me: Caller;
  onSignOut: () => void;
  className: string;
}

function LoadedChip({ scriptUrl, me, onSignOut, className }: LoadedChipProps) {
  const t = useT();
  const locale = useLocale();
  const pathname = usePathname();
  const router = useRouter();
  const bundle = useElementBundle(ACCOUNT_CHIP_TAG, scriptUrl, BUNDLE);
  const { hostRef, painted } = useChipElement(ACCOUNT_CHIP_TAG, bundle === "ready", chipAttributes(me, pathname, t), { onSignOut, onNavigate: router.push }, locale);
  return (
    <div className={className}>
      <div ref={hostRef} className="col-start-1 row-start-1 flex items-center" />
      {(bundle !== "ready" || !painted) && <ChipFallback className="col-start-1 row-start-1" me={me} onSignOut={onSignOut} />}
    </div>
  );
}
