// The front end's switches, compiled into the bundle: the image and `nix run .#dev` build the same one.

type PanelConfig = {
  /** The sign-in dialog over the panel, the platform's account chip and sign-in; concierge's bundle from /api/auth/mfe/*. Off: a 401 redirects to sign in. */
  signinOverlay: boolean;
  /** The wallet page, its bundle from /wallet/mfe/*. Off: no menu entry, the route says the wallet is unavailable. */
  walletPage: boolean;
  /** What review_archive opens as; `?grafana=true|false` switches a browser tab. */
  reviewArchiveUi: "grafana" | "legacy";
  /** `oldschool`: the kit's pre-0.31 navy look, kept to roll back to. */
  theme: "dark" | "oldschool";
};

export const PANEL: PanelConfig = {
  signinOverlay: true,
  walletPage: true,
  reviewArchiveUi: "grafana",
  theme: "dark",
};
