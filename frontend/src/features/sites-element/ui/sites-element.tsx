"use client";

import { useSyncExternalStore } from "react";

import { csrfCookieName } from "@/shared/api";
import { useLocale } from "@/shared/i18n";
import { RemoteElement } from "@/shared/mfe";

/** sites_panel's bundle, forwarded by the conductor; one script defines both elements (CONTRACT §7, §8). */
const SCRIPT = "/sites/mfe/mfe-sites.js";

const never = () => () => {};

export interface SitesElementProps {
  tag: "mfe-sites-panel" | "mfe-sites-nav";
  className?: string;
  fallback?: React.ReactNode;
}

export function SitesElement({ tag, className, fallback }: SitesElementProps) {
  const locale = useLocale();
  const csrf = useSyncExternalStore(never, csrfCookieName, () => null);
  if (csrf === null) return null;
  // Keyed on the locale: the element reads `<html lang>` when it mounts.
  return <RemoteElement key={locale} className={className} tag={tag} scriptUrl={SCRIPT} attributes={{ api: "/api/sites", base: "/sites", "csrf-cookie": csrf }} fallback={fallback} />;
}
