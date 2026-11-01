"use client";

import { useEffect, useMemo, useSyncExternalStore } from "react";

import { LOCALES, type Locale, type T, localeOf, translator } from "./translate";

const CHOSEN = "sa:locale";
const CHOSEN_EVENT = "sa:locale-chosen";
const EVENTS = ["languagechange", "storage", CHOSEN_EVENT];

const subscribe = (onChange: () => void) => {
  for (const e of EVENTS) window.addEventListener(e, onChange);
  return () => {
    for (const e of EVENTS) window.removeEventListener(e, onChange);
  };
};

function current(): Locale {
  const chosen = localStorage.getItem(CHOSEN);
  if (chosen === null) return localeOf(navigator.language);
  if (!(chosen in LOCALES)) throw new Error(`${CHOSEN}: not a locale: ${chosen}`);
  return chosen as Locale;
}

/** The export is prerendered in English; the reader's choice, else the browser's language, takes over on hydration. */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribe, current, () => "en");
}

export function chooseLocale(locale: Locale): void {
  localStorage.setItem(CHOSEN, locale);
  window.dispatchEvent(new Event(CHOSEN_EVENT));
}

/** Keeps `<html lang>` on the language the page is actually in (the export says "en"). */
export function DocumentLang() {
  const locale = useLocale();
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  return null;
}

export function useT(): T {
  const locale = useLocale();
  return useMemo(() => translator(locale), [locale]);
}
