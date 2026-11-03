"use client";

import { useResource } from "@/shared/lib/use-resource";

import { fetchAlerts } from "../api/alerts";

export function useAlerts() {
  return useResource("alerts", fetchAlerts, "alerts", { live: "alerts", focus: true });
}
