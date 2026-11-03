import { type Parser, arrayOf, bool, nullable, num, object, oneOf, str } from "@/shared/lib/parse";

import { type Alerts, type Destination, RULES, type Rule } from "./generated";

export type { Alerts, Destination, Rule };
export { RULES };

const rule = oneOf(RULES);

const rulesParser: Parser<Destination["rules"]> = (v, path) => {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${path}: expected an object`);
  return Object.fromEntries(Object.entries(v).map(([r, on]) => [rule(r, `${path} key`), bool(on, `${path}.${r}`)]));
};

const destinationParser: Parser<Destination> = object({
  id: num,
  kind: oneOf(["private", "channel", "group"] as const),
  title: nullable(str),
  topic_id: nullable(num),
  gmail_id: nullable(num),
  dead: nullable(str),
  rules: rulesParser,
});

export const alertsParser: Parser<Alerts> = object({
  enabled: bool,
  bot: nullable(str),
  rules: arrayOf(rule),
  destinations: arrayOf(destinationParser),
});

export const linkParser = object({ url: str });

/** Whether nothing reaches the caller: the bot is there, and no own chat is linked, or a destination is dead. */
export function unreached(a: Alerts): boolean {
  return a.enabled && (!a.destinations.some((d) => d.kind === "private") || a.destinations.some((d) => d.dead !== null));
}
