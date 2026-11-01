"use client";

import { useResource } from "@/shared/lib/use-resource";

import { fetchTelegram } from "../api/telegram";

export function useTelegram() {
  return useResource("telegram", fetchTelegram, "telegram", { live: "telegram", focus: true });
}
