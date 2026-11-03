import { http, ignoreBody } from "@/shared/api";

import { type Alerts, type Rule, alertsParser, linkParser } from "../model/alerts";

export function fetchAlerts(): Promise<Alerts> {
  return http.get("/api/v1/alerts", alertsParser);
}

/** The bot's `t.me` link with a one-time token: opening it links the caller's own chat. */
export async function linkTelegram(): Promise<string> {
  return (await http.send("POST", "/api/v1/alerts/telegram/link", undefined, linkParser)).url;
}

/** A channel, group or forum topic the caller administers and the bot was added to. */
export function addChat(chat: string, gmailId: number | null): Promise<Alerts> {
  return http.send("POST", "/api/v1/alerts/telegram/chats", { chat, gmail_id: gmailId }, alertsParser);
}

export function setRules(destination: number, rules: Partial<Record<Rule, boolean>>): Promise<Alerts> {
  return http.send("PUT", `/api/v1/alerts/destinations/${destination}/rules`, { rules }, alertsParser);
}

export function setGmail(destination: number, gmailId: number | null): Promise<Alerts> {
  return http.send("PUT", `/api/v1/alerts/destinations/${destination}/gmail`, { gmail_id: gmailId }, alertsParser);
}

export function testDestination(destination: number): Promise<null> {
  return http.send("POST", `/api/v1/alerts/destinations/${destination}/test`, undefined, ignoreBody);
}

export function removeDestination(destination: number): Promise<null> {
  return http.send("DELETE", `/api/v1/alerts/destinations/${destination}`, undefined, ignoreBody);
}
