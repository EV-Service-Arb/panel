/** Lowercase slug of 1–64 of [a-z0-9_-], as `sa_core::ids::is_slug` takes it. */
export function isSlug(s: string): boolean {
  return /^[a-z0-9][a-z0-9_-]{0,63}$/.test(s);
}
