"use client";

import type { ReactNode } from "react";
import { useEffect } from "react";

import { GmailsProvider } from "@/entities/gmb";
import { goToSignIn } from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useLiveSocket } from "@/shared/lib/live";

import { followTitleCount } from "../model/title-count";
import { Outline } from "./outline";
import { PanelTopBar } from "./panel-top-bar";

/** CONTRACT §9: an element's API answered 401. */
const UNAUTHENTICATED_EVENT = "sa:unauthenticated";

/** Every signed-in screen under the top bar, beside the outline; the tab's live socket and the elements' window events. */
export function ShellFrame({ onForbidden, children }: { onForbidden: () => void; children: ReactNode }) {
  const t = useT();
  useLiveSocket({ onUnauthenticated: goToSignIn, onForbidden });
  const leads = t("title.leads");
  useEffect(() => followTitleCount(window, document, leads), [leads]);
  useEffect(() => {
    window.addEventListener(UNAUTHENTICATED_EVENT, goToSignIn);
    return () => window.removeEventListener(UNAUTHENTICATED_EVENT, goToSignIn);
  }, []);
  return (
    <GmailsProvider>
      <div className="flex min-h-svh flex-col">
        <div className="sticky top-0 z-30">
          <PanelTopBar />
        </div>
        <div className="flex min-h-0 flex-1">
          {/* `hidden`, not unmounted, on a phone: the outline keeps `mfe-sites-nav` mounted */}
          <aside className="sticky top-14 hidden h-[calc(100svh-3.5rem)] w-72 shrink-0 overflow-y-auto border-r border-border p-2 md:block">
            <Outline />
          </aside>
          <main className="flex min-w-0 flex-1 flex-col">{children}</main>
        </div>
      </div>
    </GmailsProvider>
  );
}
