import { describe, expect, it } from "vitest";

import { startRouteFor } from "@/entities/session/model/access";
import { type Caller, ALIASES } from "@/entities/session/model/generated";

const holding = (alias: keyof typeof ALIASES): Caller => ({ user_id: "u", email: "e", preferred_name: "", permissions: [...ALIASES[alias]], dev_sign_in: false });

describe("the start screen", () => {
  it("is the new leads for an operator", () => {
    expect(startRouteFor(holding("sa:operator"))).toBe("/leads?stage=created");
  });

  it("is the overview for an admin", () => {
    expect(startRouteFor(holding("sa:admin"))).toBe("/overview");
  });
});
