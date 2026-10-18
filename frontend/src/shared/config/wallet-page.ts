/**
 * The wallet screen is opt-in at build time (`NEXT_PUBLIC_WALLET_PAGE=1`), like the sign-in
 * overlay: Next inlines it, so a build without the variable has no menu entry and the route
 * only says the wallet is unavailable.
 */
export const WALLET_PAGE_ENABLED = process.env.NEXT_PUBLIC_WALLET_PAGE === "1";
