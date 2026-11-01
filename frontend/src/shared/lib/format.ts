export function formatDateTime(iso: string, locale: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString(locale, { dateStyle: "short", timeStyle: "short" });
}
