/** CONTRACT §9: `mfe-sites-nav`'s count of new leads, `{count}`. */
const TITLE_COUNT_EVENT = "sa:title-count";

/**
 * `(3) Leads — Service-Arb panel` while the last count heard is above zero, the title as it
 * was otherwise and once stopped.
 */
export function followTitleCount(target: EventTarget, doc: { title: string }, label: string): () => void {
  const base = doc.title;
  const onCount = (e: Event) => {
    const count: unknown = (e as CustomEvent<{ count?: unknown }>).detail?.count;
    if (typeof count !== "number" || !Number.isInteger(count) || count < 0) throw new Error(`${TITLE_COUNT_EVENT}: not a count: ${String(count)}`);
    doc.title = count > 0 ? `(${count}) ${label} — ${base}` : base;
  };
  target.addEventListener(TITLE_COUNT_EVENT, onCount);
  return () => {
    target.removeEventListener(TITLE_COUNT_EVENT, onCount);
    doc.title = base;
  };
}
