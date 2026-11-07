import { ISSUES, type Issue, type Listed, type Tags, issueOf, issueText } from "@/entities/gmb";
import { ROUTES } from "@/shared/config/routes";
import type { T } from "@/shared/i18n";

import { type Action, type Approval, type AssistantTurn, type Mention, type Source, type Turn, finished, newId, putTurns, updateTurn } from "./chat";

// shortcut: canned answers, English only, from what the panel already read; replace with the assistant's stream once one exists.

interface Script {
  steps: string[];
  text: string;
  sources: Source[];
  actions: Omit<Action, "id" | "state">[];
}

const token = (l: Listed) => `@[${l.gmb.name ?? l.gmb.place_id}](location:${l.gmb.id})`;
const reviewsOf = (l: Listed) => `${ROUTES.reviewArchive}gmbs/${l.gmb.id}`;

/** What the canned assistant says to `prompt` about `about` (the first is the subject), from the panel's own reads. */
export function script(prompt: string, about: Listed[], all: Listed[], tags: Tags, t: T): Script {
  const asks = prompt.toLowerCase();
  const subject = about[0];
  if (subject === undefined) return triage(asks, all, tags, t);
  const archive = tags.archive?.get(subject.gmb.id);
  const site = tags.sites?.get(subject.gmb.id);
  const name = subject.gmb.name ?? subject.gmb.place_id;
  const archiveSource = { label: `Reviews · ${name}`, href: reviewsOf(subject) };
  if (/appeal|suspend|reinstat/.test(asks) || (archive !== undefined && archive.suspended > 0 && !/lead|call|book/.test(asks))) {
    if (archive === undefined || archive.suspended === 0)
      return {
        steps: [`Read Reviews board · ${name}`],
        text: `Nothing is suspended on ${token(subject)} [1]${archive?.reinstating ? `, and **${archive.reinstating}** ${archive.reinstating === 1 ? "review is" : "reviews are"} being reinstated` : ""}. No appeal to send.`,
        sources: [archiveSource],
        actions: [{ label: `Open Reviews · ${name}`, detail: "see the board yourself", href: reviewsOf(subject), done: "" }],
      };
    const n = archive.suspended;
    return {
      steps: [`Read Reviews board · ${name}`, `Read snapshots · ${n} suspended`, "Playbook · suspension, appeals"],
      text: [
        `**${n} ${n === 1 ? "review was" : "reviews were"} suspended** on ${token(subject)}${archive.rating !== null ? `; the rating shown is ${archive.rating.toFixed(1)} from ${archive.reviews ?? "?"} reviews` : ""} [1].`,
        "",
        "The playbook says to appeal within 7 days, while the snapshots are fresh [2]. I drafted one appeal per review:",
        `- each cites its snapshot as proof`,
        `- they go out from the GMB's own gmail, ${subject.gmail.gmail}`,
      ].join("\n"),
      sources: [archiveSource, { label: "Playbook · suspension", href: null }],
      actions: [
        { label: `Appeal the ${n} suspended ${n === 1 ? "review" : "reviews"}`, detail: "uses snapshots as proof · email automation sends", href: null, done: `Sent ${n} ${n === 1 ? "appeal" : "appeals"} for ${name}` },
        { label: `Open Reviews · ${name}`, detail: "see the board yourself", href: reviewsOf(subject), done: "" },
      ],
    };
  }
  if (/lead|call|book|overdue/.test(asks) && site === undefined)
    return {
      steps: [`Read ${name}`, "Read sites"],
      text: `${token(subject)} has no site, so no leads reach the panel for it [1]. Give it a place on a site, and its leads show here.`,
      sources: [{ label: "Sites", href: ROUTES.sites }],
      actions: [{ label: "Open sites", detail: "link a place to this GMB", href: ROUTES.sites, done: "" }],
    };
  if (/lead|call|book|overdue/.test(asks) && site !== undefined) {
    return {
      steps: [`Read leads · ${site.brand}/${site.location}`, "Read call outcomes · 7 d"],
      text: [
        `${token(subject)} had **${site.leads_7d} ${site.leads_7d === 1 ? "lead" : "leads"}** in the last 7 days [1]${site.overdue > 0 ? `, **${site.overdue} overdue** for a first call` : ", none overdue"}.`,
        ...(site.overdue > 0 ? ["", "Overdue leads book half as often after the first hour. I can put them at the top of today's call list."] : []),
      ].join("\n"),
      sources: [{ label: `Leads · ${site.brand}/${site.location}`, href: ROUTES.sites }],
      actions:
        site.overdue > 0
          ? [
              { label: `Put the ${site.overdue} overdue first`, detail: "today's call list · nobody else is told", href: null, done: `Moved ${site.overdue} leads to the top of today's calls` },
              { label: "Open leads", detail: "the sites panel", href: ROUTES.sites, done: "" },
            ]
          : [{ label: "Open leads", detail: "the sites panel", href: ROUTES.sites, done: "" }],
    };
  }
  if (about.length > 1)
    return {
      steps: about.map((l) => `Read ${l.gmb.name ?? l.gmb.place_id}`),
      text: [`Side by side [1]:`, ...about.map((l) => `- ${token(l)} — ${facts(l, tags).join(" · ") || "nothing read yet"}`)].join("\n"),
      sources: [archiveSource],
      actions: about.map((l) => ({ label: `Open Reviews · ${l.gmb.name ?? l.gmb.place_id}`, detail: "the archive's board", href: reviewsOf(l), done: "" })),
    };
  const issue = issueOf(subject.gmb, tags);
  const lines = facts(subject, tags).map((f) => `- ${f}`);
  return {
    steps: [`Read ${name}`, ...(archive ? ["Read Reviews board"] : []), ...(site ? ["Read leads · 7 d"] : [])],
    text: [
      `Here is what I read about ${token(subject)} [1]:`,
      ...(lines.length > 0 ? lines : ["- nothing read about it yet"]),
      "",
      issue === null ? "Nothing needs you there right now." : `What needs you first: **${issueText(issue, subject.gmb.id, tags, t).toLowerCase()}**.`,
    ].join("\n"),
    sources: [archiveSource],
    actions: [{ label: `Open Reviews · ${name}`, detail: "the archive's board", href: reviewsOf(subject), done: "" }],
  };
}

/** What the panel has read about `l`, one phrase each. */
function facts(l: Listed, tags: Tags): string[] {
  const archive = tags.archive?.get(l.gmb.id);
  const site = tags.sites?.get(l.gmb.id);
  return [
    archive?.rating != null ? `**${archive.rating.toFixed(1)}★** from ${archive.reviews ?? "?"} reviews` : null,
    archive !== undefined && archive.suspended > 0 ? `**${archive.suspended}** suspended` : null,
    archive?.failing ? "the archive's walk is failing" : null,
    site !== undefined ? `**${site.leads_7d}** leads in 7 days${site.overdue > 0 ? `, ${site.overdue} overdue` : ""}` : null,
    l.gmb.location === null ? "not on the map yet" : null,
    !l.gmb.enabled ? "switched off" : null,
  ].filter((f) => f !== null);
}

/** Every location that needs the operator, most pressing first; only those of one issue when the question names it. */
function triage(asks: string, all: Listed[], tags: Tags, t: T): Script {
  const only: Issue | null = /lead|call|overdue/.test(asks) ? "overdue" : /suspend|appeal/.test(asks) ? "suspended" : /fail/.test(asks) ? "failing" : null;
  const needing = all
    .map((l) => ({ l, issue: issueOf(l.gmb, tags) }))
    .filter((x) => x.issue !== null && x.issue !== "off" && (only === null || x.issue === only))
    .sort((a, b) => ISSUES.indexOf(a.issue!) - ISSUES.indexOf(b.issue!));
  const top = needing.slice(0, 5);
  return {
    steps: [`Read your locations · ${all.length}`, "Read Reviews archive", "Read leads"],
    text:
      all.length === 0
        ? "You have no locations yet. Add a gmail and its GMBs in the outline, and I can watch them for you."
        : needing.length === 0
          ? only === null
            ? `Nothing needs you across your **${all.length}** locations [1].`
            : `None of your **${all.length}** locations has that [1].`
          : [
              `**${needing.length} of ${all.length}** locations need you [1]:`,
              ...top.map(({ l, issue }) => `- ${token(l)} — ${issueText(issue!, l.gmb.id, tags, t).toLowerCase()}`),
              ...(needing.length > top.length ? [`- and ${needing.length - top.length} more`] : []),
            ].join("\n"),
    sources: [{ label: "Your locations", href: "/" }],
    actions: top[0] ? [{ label: `Start with ${top[0].l.gmb.name ?? top[0].l.gmb.place_id}`, detail: issueText(top[0].issue!, top[0].l.gmb.id, tags, t), href: reviewsOf(top[0].l), done: "" }] : [],
  };
}

/** Prompts worth one press for `subject`, specific to what it needs; the whole account's without one. */
export function suggestions(subject: Listed | null, tags: Tags): string[] {
  if (subject === null) return ["Which locations need me today?", "Which leads are overdue?", "Which locations have suspended reviews?"];
  const name = subject.gmb.name ?? subject.gmb.place_id;
  const archive = tags.archive?.get(subject.gmb.id);
  const site = tags.sites?.get(subject.gmb.id);
  return [
    archive !== undefined && archive.suspended > 0 ? `Appeal the ${archive.suspended} suspended reviews` : `How is ${name} doing?`,
    site !== undefined && site.overdue > 0 ? `Which of the ${site.overdue} overdue leads first?` : `Why did calls change at ${name} this week?`,
    `Draft replies to unanswered reviews`,
  ];
}

const STEP_MS = 700;
const WORD_MS = 28;

/**
 * Plays `s` into the turn: each step, then the text a few words at a time, then the actions (the first taken at
 * once under `act`). Stops where it is when `signal` aborts.
 */
export async function play(chat: string, turn: string, s: Script, approval: Approval, signal: AbortSignal): Promise<void> {
  const wait = (ms: number) =>
    new Promise<void>((done, fail) => {
      const id = setTimeout(done, ms);
      signal.addEventListener("abort", () => (clearTimeout(id), fail(signal.reason)), { once: true });
    });
  const put = (change: (t: AssistantTurn) => AssistantTurn) => updateTurn(chat, turn, change);
  try {
    for (const [i, label] of s.steps.entries()) {
      put((t) => ({ ...t, steps: [...t.steps.slice(0, i).map((x) => ({ ...x, done: true })), { label, done: false }] }));
      await wait(STEP_MS + Math.random() * STEP_MS);
    }
    put((t) => ({ ...t, state: "writing", worked: Date.now() - t.at, steps: t.steps.map((x) => ({ ...x, done: true })), sources: s.sources }));
    // A mention streams whole: half of one would show as its markup.
    const pieces = s.text.match(/@\[[^\]]*\]\([^)]*\)|\S+\s*|\s+/g) ?? [];
    for (let i = 0; i < pieces.length; i += 2) {
      const text = pieces.slice(0, i + 2).join("");
      put((t) => ({ ...t, text }));
      await wait(WORD_MS + Math.random() * WORD_MS);
    }
    const actions: Action[] = s.actions.map((a, i) => ({ ...a, id: newId(), state: approval === "act" && i === 0 && a.href === null ? "done" : "proposed" }));
    put((t) => ({ ...t, actions, state: "done", ended: Date.now() }));
    finished();
  } catch (e) {
    if (!signal.aborted) throw e;
    put((t) => ({ ...t, state: "stopped", ended: Date.now(), worked: t.worked ?? Date.now() - t.at, steps: t.steps.map((x) => ({ ...x, done: true })) }));
  }
}

// One answer runs at a time, across chats: it goes on while the panel is folded or another chat is open.
let running: AbortController | null = null;

type Question = Pick<Extract<Turn, { role: "user" }>, "text" | "mentions" | "files">;

/** `asked` put at turn `from` of `chat` (a new chat for `null`), everything after it dropped, and answered with `s`. Answers the chat's id. */
export function ask(chat: string | null, from: number, asked: Question, s: Script, approval: Approval): string {
  running?.abort();
  const at = Date.now();
  const answer: AssistantTurn = { role: "assistant", id: newId(), at, ended: null, worked: null, steps: [], text: "", sources: [], actions: [], state: "working", feedback: null };
  const id = putTurns(chat, from, [{ role: "user", id: newId(), at, ...asked }, answer]);
  const run = new AbortController();
  running = run;
  void play(id, answer.id, s, approval, run.signal).finally(() => {
    if (running === run) running = null;
  });
  return id;
}

export function stop(): void {
  running?.abort();
}

export function asMentions(listed: Listed[]): Mention[] {
  return listed.map((l) => ({ kind: "location", id: l.gmb.id, label: l.gmb.name ?? l.gmb.place_id }));
}
