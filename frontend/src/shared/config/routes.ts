export const ROUTES = {
  sources: "/sources",
  tokens: "/tokens",
  adminTokens: "/admin/tokens",
  usage: "/usage",
  account: "/account",
  alerts: "/alerts",
  wallet: "/wallet",
  /** sites_panel's page; its views are the element's to route (CONTRACT §7). */
  sites: "/sites/",
  reviewArchive: "/review_archive/",
  /** The review archive's own views: full navigations into its page. */
  reviewArchiveSettings: "/review_archive/settings",
  reviewArchiveActAs: "/review_archive/act-as",
  /** The conductor's Grafana, which the services' elements frame. */
  grafana: "/grafana/",
} as const;
