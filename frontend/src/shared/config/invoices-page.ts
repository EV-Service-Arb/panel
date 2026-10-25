/**
 * The invoices screen is opt-in at build time (`NEXT_PUBLIC_INVOICES_PAGE=1`), like the wallet:
 * Next inlines it, so a build without the variable has no menu entry and the route only says
 * that invoices are not enabled. `flake.nix` does not set it: the image ships the page dark.
 */
export const INVOICES_PAGE_ENABLED = process.env.NEXT_PUBLIC_INVOICES_PAGE === "1";
