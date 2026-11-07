"use client";

import type { ReactNode } from "react";
import { useEffect } from "react";

import { PANEL } from "@config";

import { GmailsProvider } from "@/entities/gmb";
import { seesSites, useMe } from "@/entities/session";
import { Assistant } from "@/features/assistant";
import { goToSignIn } from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useLiveSocket } from "@/shared/lib/live";

import { followTitleCount } from "../model/title-count";
import { Outline } from "./outline";
import { PanelHotbar } from "./panel-hotbar";
import { PanelTopBar } from "./panel-top-bar";

/** CONTRACT §9: an element's API answered 401. */
const UNAUTHENTICATED_EVENT = "sa:unauthenticated";

/** Every signed-in screen between the top bar and the hotbar, beside the outline; the tab's live socket and the elements' window events. */
export function ShellFrame({ onForbidden, children }: { onForbidden: () => void; children: ReactNode }) {
  const t = useT();
  const me = useMe();
  const live = useLiveSocket({ onUnauthenticated: goToSignIn, onForbidden });
  const leads = t("title.leads");
  useEffect(() => followTitleCount(window, document, leads), [leads]);
  useEffect(() => {
    window.addEventListener(UNAUTHENTICATED_EVENT, goToSignIn);
    return () => window.removeEventListener(UNAUTHENTICATED_EVENT, goToSignIn);
  }, []);
  return (
    <GmailsProvider seesSites={seesSites(me)}>
      <div className="flex h-svh flex-col">
        <PanelTopBar />
        <div className="relative flex min-h-0 flex-1">
          {/* `hidden`, not unmounted, on a phone: the outline keeps `mfe-sites-nav` mounted */}
          <aside className="hidden w-72 shrink-0 overflow-y-auto border-r border-border p-2 md:block">
            <Outline />
          </aside>
          <main className="flex min-w-0 flex-1 flex-col overflow-y-auto">{children}</main>
          {PANEL.assistant && <Assistant />}
        </div>
        <PanelHotbar live={live} />
      </div>
    </GmailsProvider>
  );
}
