import { type Parser, arrayOf, isoDay, nullable, num, object, oneOf, str } from "@/shared/lib/parse";

import { type Balance, type Kickback, LEDGER_KINDS, type Ledger, type LedgerEntry, type TokenHolders, type Tokens, type Usage } from "./generated";

export type { BalanceChange as Change, Kickback, LedgerEntry, TokenHolder as Holder, Tokens, UsageDay } from "./generated";
export { LEDGER_KINDS };

const kickbackParser: Parser<Kickback> = object({
  id: num,
  at: str,
  amount: num,
  service: nullable(str),
  key: nullable(str),
  label: nullable(str),
  by: nullable(str),
  note: nullable(str),
});

export const tokensParser: Parser<Tokens> = object({ balance: num, daily: num, cap: num, kickbacks: arrayOf(kickbackParser) });

const entryParser: Parser<LedgerEntry> = object({
  id: num,
  at: str,
  delta: num,
  kind: oneOf(LEDGER_KINDS),
  service: nullable(str),
  key: nullable(str),
  label: nullable(str),
  by: nullable(str),
  note: nullable(str),
});
export const ledgerParser: Parser<Ledger> = object({ entries: arrayOf(entryParser), next: nullable(num) });

export const usageParser: Parser<Usage> = object({ days: arrayOf(object({ day: isoDay, charges: num, tokens: num })) });

export const holdersParser: Parser<TokenHolders> = object({ users: arrayOf(object({ user_id: str, email: str, name: str, balance: num })) });

export const balanceParser: Parser<Balance> = object({ balance: num });
