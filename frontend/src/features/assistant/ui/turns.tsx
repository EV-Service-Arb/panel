"use client";

import { Button, IconButton, Spinner, Textarea, cn } from "@evinvest/uikit";
import { Check, ChevronRight, Copy, CornerDownRight, FileText, MapPin, Pencil, RotateCcw, ThumbsDown, ThumbsUp, Undo2 } from "lucide-react";
import Image from "next/image";
import Link from "next/link";
import { useEffect, useState } from "react";

import { useT } from "@/shared/i18n";

import { type Action, type AssistantTurn, type Turn, updateTurn } from "../model/chat";
import { LocationToken, RichText } from "./rich-text";

type UserTurn = Extract<Turn, { role: "user" }>;

const plain = (text: string) => text.replace(/@\[([^\]]*)\]\([^)]*\)/g, "$1").replace(/\*\*([^*]+)\*\*/g, "$1").replace(/ ?\[\d+\]/g, "");

/** Copies `text`; the icon says it worked for a moment. */
function CopyButton({ text, disabled = false }: { text: string; disabled?: boolean }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const id = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(id);
  }, [copied]);
  const label = copied ? t("assistant.copied") : t("assistant.copy");
  return (
    <IconButton size="sm" aria-label={label} title={label} disabled={disabled} onClick={() => void navigator.clipboard.writeText(text).then(() => setCopied(true))}>
      {copied ? <Check /> : <Copy />}
    </IconButton>
  );
}

const HOVER_TOOLS = "flex gap-0.5 opacity-0 transition-opacity group-hover/turn:opacity-100 group-focus-within/turn:opacity-100";

/** A question: its files and the context it was asked in above it, the asker's tools beside it on hover. */
export function UserTurnView({ turn, editing, onEdit, onEditing }: { turn: UserTurn; editing: boolean; onEdit: (text: string) => void; onEditing: (on: boolean) => void }) {
  const t = useT();
  const unnamed = turn.mentions.filter((m) => !turn.text.includes(`@${m.label}`));
  return (
    <div data-enter="rise" className="group/turn flex flex-col items-end gap-1.5 pl-10">
      {turn.files.length > 0 && (
        <div className="flex flex-wrap justify-end gap-1.5">
          {turn.files.map((f, i) =>
            f.preview !== null ? (
              <a key={i} href={f.preview.url} target="_blank" rel="noreferrer" title={f.name} className="overflow-hidden rounded-lg border border-border">
                <Image unoptimized src={f.preview.url} alt={f.name} width={f.preview.width} height={f.preview.height} className="max-h-40 w-auto max-w-60 object-cover" />
              </a>
            ) : (
              <span key={i} className="inline-flex items-center gap-1.5 rounded-lg border border-border px-2 py-1.5 text-body-s text-ink-mid">
                <FileText aria-hidden className="size-4" />
                {f.name}
              </span>
            ),
          )}
        </div>
      )}
      {unnamed.length > 0 && (
        <div className="flex flex-wrap justify-end gap-1 text-caption text-ink-soft">
          {unnamed.map((m) => (
            <span key={m.id} className="inline-flex items-center gap-1 rounded-chip border border-border-subtle px-1.5 py-0.5">
              <MapPin aria-hidden className="size-3" />
              {m.label}
            </span>
          ))}
        </div>
      )}
      {editing ? (
        <EditBox text={turn.text} onCancel={() => onEditing(false)} onSave={onEdit} />
      ) : (
        <div className="flex w-full items-start justify-end gap-1">
          <div className={cn(HOVER_TOOLS, "pt-1")}>
            <IconButton size="sm" aria-label={t("assistant.edit")} title={t("assistant.edit")} onClick={() => onEditing(true)}>
              <Pencil />
            </IconButton>
            <CopyButton text={turn.text} />
          </div>
          <div className="max-w-5/6 rounded-2xl rounded-br-md bg-hover px-3.5 py-2 text-body whitespace-pre-wrap break-words text-ink">
            <Asked text={turn.text} mentions={turn.mentions} />
          </div>
        </div>
      )}
    </div>
  );
}

function Asked({ text, mentions }: { text: string; mentions: UserTurn["mentions"] }) {
  const names = mentions.map((m) => `@${m.label}`).sort((a, b) => b.length - a.length);
  if (names.length === 0) return text;
  const split = new RegExp(`(${names.map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`);
  return text.split(split).map((part, i) => {
    const m = mentions.find((x) => `@${x.label}` === part);
    return m ? <LocationToken key={i} mention={m} className="no-underline" /> : part;
  });
}

function EditBox({ text, onCancel, onSave }: { text: string; onCancel: () => void; onSave: (text: string) => void }) {
  const t = useT();
  const [value, setValue] = useState(text);
  const save = () => value.trim() !== "" && onSave(value.trim());
  return (
    <div className="flex w-full flex-col gap-2 rounded-2xl border border-border-strong bg-card p-2.5 focus-within:ring-2 focus-within:ring-ring">
      <Textarea
        autoFocus
        rows={1}
        value={value}
        aria-label={t("assistant.edit")}
        onChange={(e) => setValue(e.target.value)}
        onFocus={(e) => e.currentTarget.setSelectionRange(e.currentTarget.value.length, e.currentTarget.value.length)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            e.preventDefault();
            onCancel();
          } else if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            save();
          }
        }}
        className="max-h-60 min-h-6 resize-none border-0 bg-transparent p-0 focus-visible:ring-0"
      />
      <div className="flex justify-end gap-1.5">
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {t("assistant.edit.cancel")}
        </Button>
        <Button size="sm" onClick={save} disabled={value.trim() === ""}>
          {t("assistant.edit.send")}
        </Button>
      </div>
    </div>
  );
}

/** An answer: what it did to get there, what it says, what it proposes, and the reader's tools under it. */
export function AssistantTurnView({ chat, turn, last, onRetry }: { chat: string; turn: AssistantTurn; last: boolean; onRetry: () => void }) {
  const t = useT();
  const running = turn.state === "working" || turn.state === "writing";
  const put = (change: (x: AssistantTurn) => AssistantTurn) => updateTurn(chat, turn.id, change);
  const feedback = (f: "up" | "down") => put((x) => ({ ...x, feedback: x.feedback === f ? null : f }));
  return (
    <div data-enter="rise" className="group/turn flex flex-col gap-2.5">
      <Work turn={turn} />
      {turn.text !== "" && <RichText text={turn.text} sources={turn.sources} writing={turn.state === "writing"} />}
      {turn.state === "stopped" && <p className="text-body-s text-ink-soft italic">{t("assistant.stopped")}</p>}
      {turn.actions.length > 0 && (
        <div data-enter="stagger" className="flex flex-col gap-1.5">
          {turn.actions.map((a, i) => (
            <ActionCard key={a.id} action={a} primary={i === 0} set={(state) => put((x) => ({ ...x, actions: x.actions.map((y) => (y.id === a.id ? { ...y, state } : y)) }))} />
          ))}
        </div>
      )}
      {!running && (
        <div className={cn("-ml-1.5 flex gap-0.5", !last && HOVER_TOOLS)}>
          <CopyButton text={plain(turn.text)} disabled={turn.text === ""} />
          {last && (
            <IconButton size="sm" aria-label={t("assistant.retry")} title={t("assistant.retry")} onClick={onRetry}>
              <RotateCcw />
            </IconButton>
          )}
          <IconButton size="sm" aria-label={t("assistant.feedback.up")} title={t("assistant.feedback.up")} aria-pressed={turn.feedback === "up"} onClick={() => feedback("up")}>
            <ThumbsUp className={cn(turn.feedback === "up" && "fill-current text-ink")} />
          </IconButton>
          <IconButton size="sm" aria-label={t("assistant.feedback.down")} title={t("assistant.feedback.down")} aria-pressed={turn.feedback === "down"} onClick={() => feedback("down")}>
            <ThumbsDown className={cn(turn.feedback === "down" && "fill-current text-ink")} />
          </IconButton>
        </div>
      )}
    </div>
  );
}

/** "Worked for 12s ›": the steps behind an answer, folded once it is written; the one running shown while it works. */
function Work({ turn }: { turn: AssistantTurn }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const working = turn.state === "working";
  const now = turn.steps.at(-1);
  if (turn.steps.length === 0)
    return working ? (
      <div role="status" className="flex items-center gap-2 text-body-s text-ink-soft">
        <Spinner className="size-3.5" />
        <span className="animate-pulse">{t("assistant.thinking")}</span>
      </div>
    ) : null;
  const seconds = Math.max(1, Math.round((turn.worked ?? 0) / 1000));
  const label = working && now ? now.label : t("assistant.worked", { time: seconds < 60 ? `${seconds}s` : `${Math.round(seconds / 60)}m` });
  return (
    <div className="flex flex-col gap-1.5">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="-ml-1 flex w-fit items-center gap-1.5 rounded-md px-1 py-0.5 text-body-s text-ink-soft outline-none hover:bg-hover hover:text-ink focus-visible:ring-2 focus-visible:ring-ring"
      >
        {working && <Spinner className="size-3.5" />}
        <span className={cn(working && "animate-pulse")}>{label}</span>
        <ChevronRight aria-hidden className={cn("size-3.5 transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <ol className="flex flex-col gap-1.5 rounded-lg border border-border px-3 py-2.5">
          {turn.steps.map((s, i) => (
            <li key={i} className="flex items-center gap-2 text-body-s text-ink-mid">
              {s.done ? <Check aria-hidden className="size-3.5 shrink-0 text-positive" /> : <Spinner className="size-3.5 shrink-0" />}
              {s.label}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

function ActionCard({ action, primary, set }: { action: Action; primary: boolean; set: (state: Action["state"]) => void }) {
  const t = useT();
  if (action.state === "done")
    return (
      <div className="flex items-center gap-2.5 rounded-lg border border-tint-positive-edge bg-tint-positive-wash px-3 py-2">
        <Check aria-hidden className="size-4 shrink-0 text-positive" />
        <span className="min-w-0 flex-1 text-body-s text-ink">{action.done}</span>
        <Button size="sm" variant="ghost" onClick={() => set("proposed")}>
          <Undo2 aria-hidden />
          {t("assistant.undo")}
        </Button>
      </div>
    );
  const look = cn(
    "group/action flex w-full items-center gap-3 rounded-lg border px-3 py-2 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
    primary ? "border-transparent bg-secondary hover:bg-hover" : "border-border hover:bg-hover",
  );
  const body = (
    <>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="text-body-s font-medium text-ink">{action.label}</span>
        <span className="truncate text-caption text-ink-soft">{action.detail}</span>
      </span>
      <CornerDownRight aria-hidden className="size-4 shrink-0 text-ink-faint opacity-0 transition-opacity group-hover/action:opacity-100" />
    </>
  );
  return action.href !== null ? (
    <Link href={action.href} className={look}>
      {body}
    </Link>
  ) : (
    <button type="button" className={look} onClick={() => set("done")}>
      {body}
    </button>
  );
}
