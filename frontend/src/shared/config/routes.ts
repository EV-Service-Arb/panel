export const ROUTES = {
  sources: "/sources",
  gmbs: "/gmbs",
  tokens: "/tokens",
  adminTokens: "/admin/tokens",
  usage: "/usage",
  account: "/account",
  wallet: "/wallet",
  /** sites_panel's page; its views are the element's to route (CONTRACT §7). */
  sites: "/sites/",
  sitesNewLeads: "/sites/leads?stage=created",
  reviewArchive: "/review_archive/",
  /** The review archive's own views: full navigations into its page. */
  reviewArchiveTelegram: "/review_archive/telegram",
  reviewArchiveActAs: "/review_archive/act-as",
} as const;
