import { http } from "@/shared/api";

import type { Ledger } from "../model/generated";
import {
  type Change,
  type Holder,
  type Tokens,
  type UsageDay,
  balanceParser,
  holdersParser,
  ledgerParser,
  tokensParser,
  usageParser,
} from "../model/tokens";

export function fetchTokens(): Promise<Tokens> {
  return http.get("/api/v1/tokens", tokensParser);
}

export function fetchLedger(before: number | null, limit: number): Promise<Ledger> {
  return http.get("/api/v1/tokens/ledger", ledgerParser, { before, limit });
}

export async function fetchUsage(): Promise<UsageDay[]> {
  return (await http.get("/api/v1/tokens/usage", usageParser)).days;
}

export function claimKickbacks(): Promise<Tokens> {
  return http.send("POST", "/api/v1/tokens/claim", undefined, tokensParser);
}

export async function fetchHolders(): Promise<Holder[]> {
  return (await http.get("/api/v1/admin/tokens", holdersParser)).users;
}

export async function changeTokens(userId: string, change: Change): Promise<number> {
  return (await http.send("POST", `/api/v1/admin/tokens/${encodeURIComponent(userId)}`, change, balanceParser)).balance;
}
