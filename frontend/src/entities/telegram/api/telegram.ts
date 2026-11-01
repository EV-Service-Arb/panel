import { http, ignoreBody } from "@/shared/api";

import { type Rule, type TelegramSettings, linkParser, settingsParser } from "../model/telegram";

export function fetchTelegram(): Promise<TelegramSettings> {
  return http.get("/api/v1/telegram", settingsParser);
}

/** The bot's `t.me` link with a one-time token: opening it links the chat. */
export async function linkTelegram(): Promise<string> {
  return (await http.send("POST", "/api/v1/telegram/link", undefined, linkParser)).url;
}

export function unlinkTelegram(): Promise<null> {
  return http.send("DELETE", "/api/v1/telegram/link", undefined, ignoreBody);
}

export function setRules(rules: Partial<Record<Rule, boolean>>): Promise<TelegramSettings> {
  return http.send("PUT", "/api/v1/telegram/rules", { rules }, settingsParser);
}
