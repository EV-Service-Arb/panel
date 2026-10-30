export const ROUTES = {
  overview: "/overview",
  leads: "/leads",
  places: "/places",
  pricing: "/pricing",
  experiments: "/experiments",
  sources: "/sources",
  more: "/more",
  usage: "/usage",
  wallet: "/wallet",
  invoices: "/invoices",
  reviewArchive: "/review_archive/",
  /** The review archive's own views: full navigations into its page. */
  reviewArchiveTokens: "/review_archive/tokens",
  reviewArchiveTelegram: "/review_archive/telegram",
  reviewArchiveActAs: "/review_archive/act-as",
  /** Its own instance behind the backend's auth proxy (spec §6); a full navigation. */
  grafana: "/grafana/",
} as const;
