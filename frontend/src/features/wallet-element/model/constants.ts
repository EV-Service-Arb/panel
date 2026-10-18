// The host side of banking's wallet element (banking, cabinet/frontend/mfe/wallet/entry.tsx,
// apiVersion 2; the contract is documented in banking sdk/wallet). Names must stay in step.

export const WALLET_TAG = "mfe-banking-wallet";
export const WALLET_API_VERSION = 2;

/**
 * Where banking publishes the bundle (`/wallet/mfe/wallet.js`, as its registry.json says).
 * Not served by the panel on purpose: the element talks to the origin it was loaded from
 * (balances, history, action pages), so a copy under the panel's origin would ask the panel for
 * `/api/wallet`. Cross-origin means the panel's CSP must name this directory in `script-src`
 * and the origin in `connect-src`; the server does that when it runs with
 * PANEL_CSP_PLATFORM_MFE=1. Without it the browser refuses the script and the page shows its
 * "unavailable" state.
 */
export const WALLET_SCRIPT = "https://evinvest.ltd/wallet/mfe/wallet.js";

/** The scope host id the element shows (main plus this scope's compartments). */
export const WALLET_SCOPE = "sa";
