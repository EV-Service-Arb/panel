import { type Parser, bool, nullable, object, oneOf, str } from "@/shared/lib/parse";

import { RULES, type Rule, type TelegramSettings } from "./generated";

export type { Rule, TelegramSettings };

const rulesParser: Parser<TelegramSettings["rules"]> = (v, path) => {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${path}: expected an object`);
  return Object.fromEntries(Object.entries(v).map(([rule, on]) => [oneOf(RULES)(rule, `${path} key`), bool(on, `${path}.${rule}`)]));
};

export const settingsParser: Parser<TelegramSettings> = object({
  enabled: bool,
  linked: bool,
  blocked: bool,
  account: nullable(str),
  rules: rulesParser,
});

export const linkParser = object({ url: str });
