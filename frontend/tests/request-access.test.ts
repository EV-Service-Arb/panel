import { describe, expect, it } from "vitest";

import { holds, safeContinue } from "@/features/request-access/model/access";

/** What Claude Code's MCP client sends playbook, its scope padded to the longest seen (375 bytes). */
const BARE =
  "/playbook_mcp/authorize?response_type=code&client_id=c1a2b3c4-d5e6-47f8-9a0b-1c2d3e4f5a6b&code_challenge=" +
  "A".repeat(43) +
  "&code_challenge_method=S256&redirect_uri=http%3A%2F%2Flocalhost%3A54321%2Fcallback&state=" +
  "B".repeat(43) +
  "&resource=https%3A%2F%2Fsa.evinvest.ltd%2Fplaybook_mcp&scope=";
const AUTHORIZE = BARE + "s".repeat(375 - BARE.length);

describe("continue", () => {
  it.each([
    ["/playbook_mcp/authorize?x=1", "/playbook_mcp/authorize?x=1"],
    ["//evil.example/x", null],
    ["/\\evil.example", null],
    ["https://evil.example/", null],
    ["/a b", null],
    ["/é", null],
    [`/${"a".repeat(511)}`, `/${"a".repeat(511)}`],
    [`/${"a".repeat(512)}`, null],
    [null, null],
    ["", null],
  ])("%s → %s, as the sign-in's return_to", (raw, want) => {
    expect(safeContinue(raw)).toBe(want);
  });

  it("a playbook authorize URL fits, inside /access, under the sign-in's return_to", () => {
    expect(AUTHORIZE.length).toBe(375);
    // As playbook sends the browser here; `/access/` is where the export's directory answers.
    const page = `/access/?${new URLSearchParams({ need: "sa:playbook:mcp:use", continue: AUTHORIZE }).toString()}`;
    expect(page.length).toBeLessThanOrEqual(512);
    expect(safeContinue(page)).toBe(page);
  });
});

describe("a need held", () => {
  it.each([
    ["sa:sites:work:read", ["sa:sites:work:read"], true],
    ["sa:sites:work:read", [], false],
    ["sa:operator", ["sa:sites:work:read", "sa:sites:work:leads:edit", "sa:sites:work:pii:see"], false],
    ["sa:operator", ["sa:sites:analysis:read", "sa:sites:work:read", "sa:sites:work:leads:edit", "sa:sites:work:pii:see"], true],
  ] as const)("%s by %j: %s", (need, permissions, want) => {
    expect(holds([...permissions], need)).toBe(want);
  });
});
