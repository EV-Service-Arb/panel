"use client";

import {
  ChatComposer,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  IconButton,
  cn,
} from "@evinvest/uikit";
import { AtSign, ChevronDown, FileText, MapPin, Paperclip, Plus, Slash, X } from "lucide-react";
import Image from "next/image";
import { type RefObject, useEffect, useImperativeHandle, useRef, useState } from "react";

import { type Listed, narrowed, useGmails } from "@/entities/gmb";
import { type T, useT } from "@/shared/i18n";

import { type Approval, type Attachment, type Mention, setApproval } from "../model/chat";

export interface Draft {
  text: string;
  mentions: Mention[];
  files: Attachment[];
  /** The page's location left out of this question. */
  subjectOff: boolean;
}

export interface ComposerHandle {
  focus: () => void;
  attach: (files: readonly File[]) => void;
  mention: () => void;
}

// A draft outlives the panel folding and switching chats, not a reload.
const drafts = new Map<string, Draft>();
const EMPTY: Draft = { text: "", mentions: [], files: [], subjectOff: false };

const COMMANDS = [
  { id: "status", prompt: (s: string | null) => (s ? `How is @${s} doing?` : "Which locations need me today?") },
  { id: "appeal", prompt: (s: string | null) => (s ? `Appeal the suspended reviews at @${s}` : "Appeal the suspended reviews") },
  { id: "leads", prompt: (s: string | null) => (s ? `Which overdue leads at @${s} first?` : "Which overdue leads first?") },
  { id: "triage", prompt: () => "Which locations need me today?" },
] as const;

type Picking = { kind: "mention"; query: string; from: number } | { kind: "command"; query: string };

export function Composer({
  handle,
  chat,
  subject,
  streaming,
  approval,
  onSend,
  onStop,
  onEditLast,
}: {
  handle: RefObject<ComposerHandle | null>;
  /** Whose draft this is; `null` for a chat not started. */
  chat: string | null;
  subject: Listed | null;
  streaming: boolean;
  approval: Approval;
  onSend: (draft: Draft) => void;
  onStop: () => void;
  onEditLast: () => void;
}) {
  const t = useT();
  const key = chat ?? "";
  const [draft, setDraftState] = useState<Draft>(() => drafts.get(key) ?? EMPTY);
  const [picking, setPicking] = useState<Picking | null>(null);
  const [active, setActive] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const field = () => root.current?.querySelector("textarea") ?? null;
  // A chat opened, the panel unfolded: either way the next thing is to type.
  useEffect(() => field()?.focus(), []);

  const setDraft = (change: (d: Draft) => Draft) =>
    setDraftState((d) => {
      const next = change(d);
      drafts.set(key, next);
      return next;
    });

  const attach = (files: readonly File[]) => {
    for (const f of files) void attachmentOf(f).then((a) => setDraft((d) => ({ ...d, files: [...d.files, a] })));
  };
  const insert = (text: string) => {
    const el = field();
    const at = el?.selectionStart ?? draft.text.length;
    const before = draft.text.slice(0, at);
    const pad = before === "" || /\s$/.test(before) ? "" : " ";
    setDraft((d) => ({ ...d, text: `${before}${pad}${text}${d.text.slice(at)}` }));
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(at + pad.length + text.length, at + pad.length + text.length);
      follow(el);
    });
  };
  useImperativeHandle(handle, () => ({ focus: () => field()?.focus(), attach, mention: () => insert("@") }));

  const gmails = useGmails();
  const all = gmails.status === "ok" ? gmails.data : [];
  const locations = picking?.kind === "mention" ? narrowed(all, null, picking.query).slice(0, 7) : [];
  const commands = picking?.kind === "command" ? COMMANDS.filter((c) => c.id.startsWith(picking.query.toLowerCase())) : [];
  const options = picking?.kind === "mention" ? locations.length : commands.length;

  /** What the text before the caret is starting: `@guil` a mention, a leading `/ap` a command. */
  function follow(el: HTMLTextAreaElement | null) {
    if (el === null) return;
    const before = el.value.slice(0, el.selectionStart);
    const at = /(^|\s)@([^\s@]{0,40})$/.exec(before);
    const slash = /^\/(\w{0,20})$/.exec(before);
    const next: Picking | null = at ? { kind: "mention", query: at[2]!, from: before.length - at[2]!.length - 1 } : slash ? { kind: "command", query: slash[1]! } : null;
    setPicking((p) => (p?.kind === next?.kind && p?.query === next?.query ? p : next));
    if (next?.query !== picking?.query) setActive(0);
  }

  const choose = (i: number) => {
    const el = field();
    if (picking?.kind === "mention") {
      const l = locations[i];
      if (l === undefined) return;
      const label = l.gmb.name ?? l.gmb.place_id;
      const caret = el?.selectionStart ?? draft.text.length;
      const text = `${draft.text.slice(0, picking.from)}@${label} ${draft.text.slice(caret)}`;
      setDraft((d) => ({ ...d, text, mentions: d.mentions.some((m) => m.id === l.gmb.id) ? d.mentions : [...d.mentions, { kind: "location", id: l.gmb.id, label }] }));
      const end = picking.from + label.length + 2;
      requestAnimationFrame(() => el?.setSelectionRange(end, end));
    } else if (picking?.kind === "command") {
      const c = commands[i];
      if (c === undefined) return;
      const s = subject && !draft.subjectOff ? (subject.gmb.name ?? subject.gmb.place_id) : null;
      const text = c.prompt(s);
      setDraft((d) => ({
        ...d,
        text,
        mentions: subject !== null && s !== null && !d.mentions.some((m) => m.id === subject.gmb.id) ? [...d.mentions, { kind: "location", id: subject.gmb.id, label: s }] : d.mentions,
      }));
      requestAnimationFrame(() => el?.setSelectionRange(text.length, text.length));
    }
    setPicking(null);
  };

  const send = (text: string) => {
    onSend({ ...draft, text });
    setDraft(() => EMPTY);
    setPicking(null);
  };

  const subjectShown = subject !== null && !draft.subjectOff && !draft.mentions.some((m) => m.id === subject.gmb.id);
  const name = subject ? (subject.gmb.name ?? subject.gmb.place_id) : null;
  const chips = subjectShown || draft.mentions.length > 0 || draft.files.length > 0;

  return (
    <div ref={root} className="relative">
      {picking !== null && options > 0 && (
        <div
          role="listbox"
          id="assistant-picker"
          aria-label={t(picking.kind === "mention" ? "assistant.pick.locations" : "assistant.pick.commands")}
          className="absolute inset-x-0 bottom-full z-10 mb-1.5 flex flex-col overflow-hidden rounded-lg border border-border bg-popover p-1 shadow-overlay"
          onMouseDown={(e) => e.preventDefault()}
        >
          <div className="px-2 pt-1 pb-1.5 text-caption text-ink-soft">{t(picking.kind === "mention" ? "assistant.pick.locations" : "assistant.pick.commands")}</div>
          {picking.kind === "mention"
            ? locations.map((l, i) => (
                <Option key={l.gmb.id} active={i === active} onHover={() => setActive(i)} onChoose={() => choose(i)}>
                  <MapPin aria-hidden className="size-4 shrink-0 text-ink-soft" />
                  <span className="truncate text-ink">{l.gmb.name ?? l.gmb.place_id}</span>
                  <span className="ml-auto truncate pl-3 text-caption text-ink-faint">{l.gmb.address ?? l.gmail.gmail}</span>
                </Option>
              ))
            : commands.map((c, i) => (
                <Option key={c.id} active={i === active} onHover={() => setActive(i)} onChoose={() => choose(i)}>
                  <Slash aria-hidden className="size-4 shrink-0 text-ink-soft" />
                  <span className="text-ink">{t(`assistant.command.${c.id}`)}</span>
                  <span className="ml-auto pl-3 font-mono text-caption text-ink-faint">/{c.id}</span>
                </Option>
              ))}
        </div>
      )}
      <ChatComposer
        value={draft.text}
        onValueChange={(text) => setDraft((d) => ({ ...d, text }))}
        onSend={send}
        streaming={streaming}
        onStop={onStop}
        placeholder={name ? t("assistant.placeholder.about", { name: name.length > 28 ? `${name.slice(0, 27).trimEnd()}…` : name }) : t("assistant.placeholder")}
        labels={{ field: t("assistant.field"), send: t("assistant.send"), stop: t("assistant.stop"), attach: t("assistant.attach") }}
        attachments={
          chips ? (
            <>
              {subjectShown && (
                <Chip title={t("assistant.context.page")} onRemove={() => setDraft((d) => ({ ...d, subjectOff: true }))} removeLabel={t("assistant.context.remove", { name: name! })}>
                  <MapPin aria-hidden className="size-3.5 shrink-0 text-ink-soft" />
                  <span className="max-w-48 truncate">{name}</span>
                  <span className="shrink-0 text-ink-faint">· {t("assistant.context.here")}</span>
                </Chip>
              )}
              {draft.mentions.map((m) => (
                <Chip key={m.id} onRemove={() => setDraft((d) => ({ ...d, mentions: d.mentions.filter((x) => x.id !== m.id) }))} removeLabel={t("assistant.context.remove", { name: m.label })}>
                  <MapPin aria-hidden className="size-3.5 shrink-0 text-ink-soft" />
                  <span className="max-w-48 truncate">{m.label}</span>
                </Chip>
              ))}
              {draft.files.map((f, i) => (
                <Chip key={i} onRemove={() => setDraft((d) => ({ ...d, files: d.files.filter((_, j) => j !== i) }))} removeLabel={t("assistant.context.remove", { name: f.name })}>
                  {f.preview !== null ? <Image unoptimized src={f.preview.url} alt="" width={16} height={16} className="size-4 rounded-sm object-cover" /> : <FileText aria-hidden className="size-3.5 text-ink-soft" />}
                  <span className="max-w-32 truncate">{f.name}</span>
                </Chip>
              ))}
            </>
          ) : undefined
        }
        tools={
          <>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <IconButton size="sm" aria-label={t("assistant.add")} title={t("assistant.add")}>
                  <Plus />
                </IconButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start">
                <DropdownMenuItem onSelect={() => fileInput.current?.click()}>
                  <Paperclip aria-hidden />
                  {t("assistant.attach")}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => requestAnimationFrame(() => insert("@"))}>
                  <AtSign aria-hidden />
                  {t("assistant.mention")}
                </DropdownMenuItem>
                {subject !== null && draft.subjectOff && (
                  <DropdownMenuItem onSelect={() => setDraft((d) => ({ ...d, subjectOff: false }))}>
                    <MapPin aria-hidden />
                    {t("assistant.context.add", { name: name! })}
                  </DropdownMenuItem>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
            <ApprovalChip approval={approval} t={t} />
          </>
        }
        hint={draft.text === "" ? t("assistant.hint") : undefined}
        onPaste={(e) => {
          const files = [...e.clipboardData.files];
          if (files.length === 0) return;
          e.preventDefault();
          attach(files);
        }}
        onKeyDownCapture={(e) => {
          const step = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
          const picks = picking !== null && options > 0;
          const taken = picks && (step !== 0 || (e.key === "Enter" && !e.shiftKey) || e.key === "Tab" || e.key === "Escape");
          if (taken) {
            e.preventDefault();
            e.stopPropagation();
            if (step !== 0) setActive((a) => (a + step + options) % options);
            else if (e.key === "Escape") setPicking(null);
            else choose(active);
          } else if (e.key === "ArrowUp" && draft.text === "" && !e.shiftKey) {
            e.preventDefault();
            onEditLast();
          }
        }}
        onKeyUp={(e) => follow(e.target as HTMLTextAreaElement)}
        onClick={(e) => e.target instanceof HTMLTextAreaElement && follow(e.target)}
        onInput={(e) => follow(e.target as HTMLTextAreaElement)}
      />
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          attach([...(e.target.files ?? [])]);
          e.target.value = "";
        }}
      />
    </div>
  );
}

function Option({ active, onHover, onChoose, children }: { active: boolean; onHover: () => void; onChoose: () => void; children: React.ReactNode }) {
  return (
    <div
      role="option"
      aria-selected={active}
      onMouseMove={onHover}
      onClick={onChoose}
      className={cn("flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-body-s", active && "bg-hover")}
    >
      {children}
    </div>
  );
}

function Chip({ title, onRemove, removeLabel, children }: { title?: string; onRemove: () => void; removeLabel: string; children: React.ReactNode }) {
  return (
    <span title={title} className="inline-flex h-6 max-w-full items-center gap-1 rounded-chip border border-border bg-background pr-0.5 pl-1.5 text-body-s text-ink">
      {children}
      <button type="button" aria-label={removeLabel} onClick={onRemove} className="ml-0.5 shrink-0 rounded-sm p-0.5 text-ink-faint outline-none hover:bg-hover hover:text-ink focus-visible:ring-2 focus-visible:ring-ring">
        <X aria-hidden className="size-3" />
      </button>
    </span>
  );
}

/** Whether the assistant waits for a press before acting. */
function ApprovalChip({ approval, t }: { approval: Approval; t: T }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={`${t("assistant.approval")}: ${t(`assistant.approval.${approval}`)}`}
        className="inline-flex h-6.5 items-center gap-1 rounded-chip py-1 pr-1.5 pl-2 text-body-s text-ink-mid outline-none hover:bg-hover focus-visible:ring-2 focus-visible:ring-ring"
      >
        {t(`assistant.approval.${approval}`)}
        <ChevronDown aria-hidden className="size-3.5" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        <DropdownMenuLabel>{t("assistant.approval")}</DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuRadioGroup value={approval} onValueChange={(v) => setApproval(v as Approval)}>
          {(["ask", "act"] as const).map((a) => (
            <DropdownMenuRadioItem key={a} value={a} className="items-start">
              <span className="flex flex-col">
                <span>{t(`assistant.approval.${a}`)}</span>
                <span className="text-caption text-ink-soft">{t(`assistant.approval.${a}.hint`)}</span>
              </span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const PREVIEW_EDGE = 480;

/** A file as a question carries it: an image downscaled to a preview this browser keeps. */
async function attachmentOf(file: File): Promise<Attachment> {
  if (!file.type.startsWith("image/")) return { name: file.name, preview: null, bytes: file.size };
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, PREVIEW_EDGE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(bitmap.width * scale);
  canvas.height = Math.round(bitmap.height * scale);
  const ctx = canvas.getContext("2d");
  if (ctx === null) throw new Error("no 2d canvas context");
  ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  return { name: file.name || "image.png", preview: { url: canvas.toDataURL("image/jpeg", 0.8), width: canvas.width, height: canvas.height }, bytes: file.size };
}
