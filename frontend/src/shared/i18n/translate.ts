import { CATALOGUES, type MessageKey } from "./catalogue";

/** Each language by its own name. */
export const LOCALES = { en: "English", ru: "Русский" } as const;
export type Locale = keyof typeof LOCALES;
type Vars = Record<string, string | number>;
export type T = (key: MessageKey, vars?: Vars) => string;


/** The panel speaks the reader's browser language when it is Russian, English otherwise. */
export function localeOf(language: string | undefined): Locale {
  return language?.toLowerCase().startsWith("ru") ? "ru" : "en";
}

export function translator(locale: Locale): T {
  const messages: Record<MessageKey, string> = CATALOGUES[locale];
  return (key, vars) => {
    const text = messages[key];
    return vars ? text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in vars ? String(vars[name]) : whole)) : text;
  };
}
