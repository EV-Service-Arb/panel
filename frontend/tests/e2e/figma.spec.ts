import { test } from "@playwright/test";
import { defineFigmaSuite } from "@evinvest/figma-render";

// The conductor's own screens. Not `/sites`, `/review_archive`: other services' bundles the stub does
// not serve; nor `/account`, `/wallet`: the platform's, which refuses a localhost origin.
// `nextjs-portal` is `next dev`'s indicator.
defineFigmaSuite(
  { test },
  ["/", "/sources/", "/usage/", "/tokens/", "/alerts/", "/admin/tokens/", "/access/?need=sa:playbook:mcp:use"],
  { hide: ["nextjs-portal"] },
);
