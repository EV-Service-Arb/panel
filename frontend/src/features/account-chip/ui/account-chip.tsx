"use client";

import { cn } from "@evinvest/uikit";
import { ACCOUNT_CHIP_TAG } from "@evinvest/concierge";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useState } from "react";

import type { Caller } from "@/entities/session";
import { PLATFORM_MFE_ENABLED, platformMfe } from "@/shared/api";
import { useLocale, useT } from "@/shared/i18n";
import { useElementBundle } from "@/shared/mfe";

import { CHIP_API_VERSION, CHIP_SCRIPT, CHIP_TAG, chipAttributes } from "../model/attributes";
import { ChipFallback } from "./chip-fallback";
import { useChipElement } from "./use-chip-element";

// A stable object: the loader's effect is keyed on its fields.
const BUNDLE = { minApiVersion: CHIP_API_VERSION, timeoutMs: 8000 } as const;

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
  const source = useChipSource();
  const cell = cn("grid min-h-10.5 min-w-44.5 items-center justify-items-end", className);
  // Registry not answered yet, or it gave no chip: the fallback alone, the way a bundle that
  // failed to load looks.
  if (source === null) {
    return (
      <div className={cell}>
        <ChipFallback className="col-start-1 row-start-1" me={me} onSignOut={onSignOut} />
      </div>
    );
  }
  return <LoadedChip source={source} me={me} onSignOut={onSignOut} className={cell} />;
}

interface ChipSource {
  tag: string;
  scriptUrl: string;
}

const SAME_ORIGIN_SOURCE: ChipSource = { tag: CHIP_TAG, scriptUrl: CHIP_SCRIPT };

/**
 * Where the chip's bundle comes from. Off (the default), the cabinet's chip through the panel's
 * proxy, known at once. On (`NEXT_PUBLIC_MFE_PLATFORM=1`), the concierge chip the registry names;
 * it takes the same host-mode attributes. null until the registry answers, and for good if it
 * does not.
 */
function useChipSource(): ChipSource | null {
  const [source, setSource] = useState<ChipSource | null>(PLATFORM_MFE_ENABLED ? null : SAME_ORIGIN_SOURCE);
  useEffect(() => {
    if (!PLATFORM_MFE_ENABLED) return;
    let stale = false;
    void platformMfe.scriptUrl("accountChip", CHIP_SCRIPT, BUNDLE.timeoutMs).then(scriptUrl => {
      if (!stale && scriptUrl !== null) setSource({ tag: ACCOUNT_CHIP_TAG, scriptUrl });
    });
    return () => {
      stale = true;
    };
  }, []);
  return source;
}

interface LoadedChipProps {
  source: ChipSource;
  me: Caller;
  onSignOut: () => void;
  className: string;
}

function LoadedChip({ source, me, onSignOut, className }: LoadedChipProps) {
  const t = useT();
  const locale = useLocale();
  const pathname = usePathname();
  const router = useRouter();
  const bundle = useElementBundle(source.tag, source.scriptUrl, BUNDLE);
  const { hostRef, painted } = useChipElement(source.tag, bundle === "ready", chipAttributes(me, pathname, t), { onSignOut, onNavigate: router.push }, locale);
  return (
    <div className={className}>
      <div ref={hostRef} className="col-start-1 row-start-1 flex items-center" />
      {(bundle !== "ready" || !painted) && <ChipFallback className="col-start-1 row-start-1" me={me} onSignOut={onSignOut} />}
    </div>
  );
}
