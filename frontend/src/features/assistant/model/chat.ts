"use client";

import { useSyncExternalStore } from "react";

/** Something a turn is about: a GMB today; other kinds (a lead, a site) join with their own picker. */
export interface Mention {
  kind: "location";
  id: number;
  label: string;
}

export interface Attachment {
  name: string;
  /** An image's preview, downscaled (it is kept in this browser); `null` for any other file. */
  preview: { url: string; width: number; height: number } | null;
  bytes: number;
}

export interface Step {
  label: string;
  done: boolean;
}

export interface Action {
  id: string;
  label: string;
  detail: string;
  /** Where it goes, for one that opens a page instead of doing something. */
  href: string | null;
  /** Said once it is done: "Appealed 4 reviews". */
  done: string;
  state: "proposed" | "done";
}

export interface Source {
  label: string;
  /** `null`: read, but nowhere in the panel to open. */
  href: string | null;
}

export type Turn =
  | { role: "user"; id: string; at: number; text: string; mentions: Mention[]; files: Attachment[] }
  | {
      role: "assistant";
      id: string;
      at: number;
      /** When the answer stopped growing; `null` while it runs. */
      ended: number | null;
      /** How long its steps took, once they are over. */
      worked: number | null;
      steps: Step[];
      /** `**bold**`, `- ` items, `@[label](location:id)` and `[n]` for `sources[n-1]`. */
      text: string;
      sources: Source[];
      actions: Action[];
      state: "working" | "writing" | "done" | "stopped";
      feedback: "up" | "down" | null;
    };

export type AssistantTurn = Extract<Turn, { role: "assistant" }>;

export interface Chat {
  id: string;
  /** `null` until named by its first question, or renamed. */
  title: string | null;
  updated: number;
  turns: Turn[];
}

/** How the panel shows: a launcher, a window over the page, or a column beside it. */
export type Placement = "folded" | "floating" | "docked";
/** `ask`: actions wait for a press; `act`: the first is taken at once, and can be undone. */
export type Approval = "ask" | "act";

interface State {
  chats: Chat[];
  current: string | null;
  placement: Placement;
  /** Where it opens back to from the launcher. */
  opens: Exclude<Placement, "folded">;
  width: number;
  approval: Approval;
  /** An answer finished while folded. */
  unread: boolean;
}

const KEY = "sa:assistant";
export const WIDTH = { min: 340, max: 760, initial: 420 } as const;
const INITIAL: State = { chats: [], current: null, placement: "folded", opens: "floating", width: WIDTH.initial, approval: "ask", unread: false };

let state: State | null = null;
const listeners = new Set<() => void>();

function load(): State {
  const kept = localStorage.getItem(KEY);
  if (kept === null) return INITIAL;
  const read = JSON.parse(kept) as State;
  // An answer a reload cut short stays cut.
  const chats = read.chats.map((c) => ({ ...c, turns: c.turns.map((t) => (t.role === "assistant" && t.ended === null ? { ...t, state: "stopped" as const, ended: t.at } : t)) }));
  return { ...read, chats };
}

function current(): State {
  state ??= load();
  return state;
}

function set(change: (s: State) => State): void {
  state = change(current());
  localStorage.setItem(KEY, JSON.stringify(state));
  for (const l of listeners) l();
}

const subscribe = (onChange: () => void) => {
  listeners.add(onChange);
  return () => listeners.delete(onChange);
};

export function useAssistant(): State {
  return useSyncExternalStore(subscribe, current, () => INITIAL);
}

export const newId = () => crypto.randomUUID();

export function place(placement: Placement): void {
  set((s) => ({ ...s, placement, ...(placement !== "folded" && { opens: placement, unread: false }) }));
}

export function setWidth(width: number): void {
  set((s) => ({ ...s, width: Math.round(Math.min(WIDTH.max, Math.max(WIDTH.min, width))) }));
}

export function setApproval(approval: Approval): void {
  set((s) => ({ ...s, approval }));
}

export function openChat(id: string | null): void {
  set((s) => ({ ...s, current: id }));
}

export function removeChat(id: string): void {
  set((s) => ({ ...s, chats: s.chats.filter((c) => c.id !== id), current: s.current === id ? null : s.current }));
}

/** A removed chat back where it was in the history. */
export function restoreChat(chat: Chat): void {
  set((s) => ({ ...s, chats: [...s.chats, chat].sort((a, b) => b.updated - a.updated), current: chat.id }));
}

export function renameChat(id: string, title: string): void {
  set((s) => ({ ...s, chats: s.chats.map((c) => (c.id === id ? { ...c, title: title.trim() || null } : c)) }));
}

/** The chat's turns from `from` on replaced by `turns`; a chat made for them when `chat` is `null`. Answers the chat's id. */
export function putTurns(chat: string | null, from: number, turns: Turn[]): string {
  const id = chat ?? newId();
  set((s) => {
    const existing = s.chats.find((c) => c.id === id);
    const first = turns.find((t) => t.role === "user");
    const made: Chat = existing ?? { id, title: first?.role === "user" ? titleOf(first.text) : null, updated: Date.now(), turns: [] };
    const next = { ...made, updated: Date.now(), turns: [...made.turns.slice(0, from), ...turns] };
    return { ...s, current: id, chats: [next, ...s.chats.filter((c) => c.id !== id)] };
  });
  return id;
}

export function updateTurn(chat: string, turn: string, change: (t: AssistantTurn) => AssistantTurn): void {
  set((s) => ({
    ...s,
    chats: s.chats.map((c) => (c.id === chat ? { ...c, turns: c.turns.map((t) => (t.id === turn && t.role === "assistant" ? change(t) : t)) } : c)),
  }));
}

/** An answer just finished: told on the launcher if nobody was looking. */
export function finished(): void {
  set((s) => ({ ...s, unread: s.placement === "folded" }));
}

function titleOf(text: string): string {
  const line = text.replace(/\s+/g, " ").replace(/(^|\s)@/g, "$1").trim();
  return line.length > 48 ? `${line.slice(0, 47).trimEnd()}…` : line;
}
