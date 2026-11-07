"use client";

import { cn } from "@evinvest/uikit";
import { MapPin } from "lucide-react";
import Link from "next/link";
import { Fragment, type ReactNode } from "react";

import { setOnMap } from "@/entities/gmb";

import type { Mention, Source } from "../model/chat";

const INLINE = /(\*\*[^*]+\*\*|@\[[^\]]*\]\(location:\d+\)|\[\d+\])/g;

/** An answer's text: paragraphs and `- ` lists, bold, location tokens and numbered sources; a caret after the last word while `writing`. */
export function RichText({ text, sources, writing }: { text: string; sources: readonly Source[]; writing: boolean }) {
  const blocks: { list: boolean; lines: string[] }[] = [];
  for (const line of text.split("\n")) {
    const list = line.startsWith("- ");
    if (line.trim() === "") blocks.push({ list: false, lines: [] });
    else if (blocks.at(-1)?.list === list && (list || blocks.at(-1)!.lines.length > 0)) blocks.at(-1)!.lines.push(list ? line.slice(2) : line);
    else blocks.push({ list, lines: [list ? line.slice(2) : line] });
  }
  const shown = blocks.filter((b) => b.lines.length > 0);
  const caret = writing && <span aria-hidden className="ml-0.5 inline-block h-4 w-0.5 translate-y-0.5 animate-pulse rounded-full bg-ink-mid" />;
  return (
    <div className="flex flex-col gap-2 text-body leading-relaxed text-ink">
      {shown.map((b, i) => {
        const last = i === shown.length - 1;
        return b.list ? (
          <ul key={i} className="flex list-disc flex-col gap-1 pl-5 marker:text-ink-faint">
            {b.lines.map((l, j) => (
              <li key={j}>
                <Inline text={l} sources={sources} />
                {last && j === b.lines.length - 1 && caret}
              </li>
            ))}
          </ul>
        ) : (
          <p key={i}>
            {b.lines.map((l, j) => (
              <Fragment key={j}>
                {j > 0 && <br />}
                <Inline text={l} sources={sources} />
              </Fragment>
            ))}
            {last && caret}
          </p>
        );
      })}
      {shown.length === 0 && caret && <p>{caret}</p>}
    </div>
  );
}

function Inline({ text, sources }: { text: string; sources: readonly Source[] }) {
  const parts = text.split(INLINE);
  return parts.map((part, i): ReactNode => {
    if (part.startsWith("**") && part.endsWith("**") && part.length > 4) return <strong key={i} className="font-semibold">{part.slice(2, -2)}</strong>;
    const mention = /^@\[([^\]]*)\]\(location:(\d+)\)$/.exec(part);
    if (mention) return <LocationToken key={i} mention={{ kind: "location", id: Number(mention[2]), label: mention[1]! }} />;
    const cite = /^\[(\d+)\]$/.exec(part);
    const source = cite && sources[Number(cite[1]) - 1];
    if (source) return <Citation key={i} n={Number(cite[1])} source={source} />;
    // A number keeps to the word it cites.
    return /^\[\d+\]$/.test(parts[i + 1] ?? "") ? part.replace(/ $/, "\u00a0") : part;
  });
}

/** A location named in a turn: opens its card on the map. A link, not a button: a long name wraps like the words around it. */
export function LocationToken({ mention, className }: { mention: Mention; className?: string }) {
  return (
    <Link
      href="/"
      title={mention.label}
      onClick={() => setOnMap({ peeked: mention.id })}
      className={cn(
        "rounded-sm px-0.5 font-medium text-ink underline decoration-border-strong underline-offset-3 outline-none wrap-anywhere hover:bg-hover hover:decoration-ink-soft focus-visible:ring-2 focus-visible:ring-ring",
        className,
      )}
    >
      <MapPin aria-hidden className="mr-1 inline size-3.5 -translate-y-px text-ink-soft" />
      {mention.label}
    </Link>
  );
}

function Citation({ n, source }: { n: number; source: Source }) {
  const look = "mx-0.5 inline-flex size-4.5 -translate-y-px items-center justify-center rounded-full bg-tint-neutral-fill align-middle text-caption leading-none font-medium text-ink-mid";
  if (source.href === null)
    return (
      <span title={source.label} className={look}>
        {n}
      </span>
    );
  return (
    <Link href={source.href} title={source.label} aria-label={source.label} className={cn(look, "outline-none hover:bg-primary hover:text-on-primary focus-visible:ring-2 focus-visible:ring-ring")}>
      {n}
    </Link>
  );
}
