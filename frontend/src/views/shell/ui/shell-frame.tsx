"use client";

import { AppShell } from "@evinvest/uikit";
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";

import { useMe } from "@/entities/session";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { NavMarksProvider } from "@/shared/lib/nav-marks";
import { useTitleCount } from "@/shared/lib/use-title-count";

import { useMarks } from "../model/use-marks";
import { panelNav } from "./nav-items";
import { PanelRail } from "./panel-rail";
import { PanelTabs } from "./panel-tabs";
import { PanelTopBar } from "./panel-top-bar";

/** The kit's shell around every signed-in screen, with the nav's marks and the tab title's count. */
export function ShellFrame({ children }: { children: ReactNode }) {
  const t = useT();
  const me = useMe();
  const marks = useMarks(me.user_id, usePathname());
  useTitleCount(marks.away, t("nav.leads"));
  const nav = panelNav(t, me, marks);
  return (
    <NavMarksProvider value={{ [ROUTES.experiments]: marks.experiments, [ROUTES.sources]: marks.sources }}>
      <AppShell breakpoint="md" rail={<PanelRail groups={nav.groups} footer={nav.footer} />} topBar={<PanelTopBar />} tabBar={<PanelTabs items={nav.tabs} />}>
        {children}
      </AppShell>
    </NavMarksProvider>
  );
}
