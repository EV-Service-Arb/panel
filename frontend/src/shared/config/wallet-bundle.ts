// The host side of banking's wallet bundle (banking, wallet/frontend/mfe/wallet/entry.tsx,
// apiVersion 3; the contract is banking docs/WALLET-INTENTS.md). One bundle defines both
// `<mfe-banking-wallet>` and `<mfe-banking-pay-invoice>`. Names must stay in step.

/** The one origin that may carry out a money action and report its result. */
export const WALLET_ORIGIN = "https://evinvest.ltd";

/**
 * Where banking publishes the bundle (`/wallet/mfe/wallet.js`, as its registry.json says).
 * Not served by the panel on purpose: the elements talk to the origin they were loaded from
 * (balances, history, action pages), so a copy under the panel's origin would ask the panel for
 * `/api/wallet`. Cross-origin means the panel's CSP must name this directory in `script-src`
 * and the origin in `connect-src`; the server does that when it runs with
 * PANEL_CSP_PLATFORM_MFE=1. Without it the browser refuses the script and the page shows its
 * "unavailable" state.
 */
export const WALLET_SCRIPT = `${WALLET_ORIGIN}/wallet/mfe/wallet.js`;

/**
 * The bytes of `WALLET_SCRIPT` this panel runs (banking WALLET-INPAGE-SPEC control (g): the
 * in-page pay form runs in this page). The sha256 of banking's `npm run --silent mfe:pins --
 * wallet` (`wallet.js=<hex>`) as SRI. banking's registry carries no integrity, so a release that
 * changes wallet.js turns the wallet and the pay buttons "unavailable" until this is updated.
 */
export const WALLET_SCRIPT_INTEGRITY = "sha256-IsAtO+QmNM8MSKLYvK+vVKtRTrpCl3w6Je8M/9bu9rk=";

/** The lowest `static apiVersion` of the bundle's elements that speaks this contract. */
export const WALLET_API_VERSION = 2;

/** The scope host id the elements show (main plus this scope's compartments). */
export const WALLET_SCOPE = "sa";
