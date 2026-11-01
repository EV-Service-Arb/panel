"use client";

import { useResource } from "@/shared/lib/use-resource";

import { fetchTokens } from "../api/tokens";

/** The caller's tokens, read on mount and again on focus: no live topic tells of a charge. */
export function useTokens() {
  return useResource("tokens", fetchTokens, "tokens", { focus: true });
}
