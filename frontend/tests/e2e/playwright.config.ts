import { defineConfig, devices } from "@playwright/test";

const port = (name: string): number => {
  const value = process.env[name];
  if (!value) throw new Error(`${name}: the flake's localEnv sets it; run through \`nix run .#figma\``);
  return Number(value);
};
const STUB = port("FIGMA_STUB_PORT");
const WEB = port("FIGMA_WEB_PORT");

// `next dev` over the stub, as an admin, so every screen of the conductor's own has made-up data to
// show. Run through the flake (`nix run .#figma`), which supplies `@playwright/test` with its pinned
// browsers.
export default defineConfig({
  testDir: ".",
  outputDir: "../../tmp/playwright",
  reporter: "list",
  use: {
    baseURL: `http://localhost:${WEB}`, // `next dev` refuses its dev resources to any other host, so the page never hydrates
    deviceScaleFactor: 1,
    // What the stub's `/auth/login` sets.
    storageState: {
      cookies: [
        { name: "sa_session", value: "stub", domain: "localhost", path: "/", expires: -1, httpOnly: true, secure: false, sameSite: "Lax" },
        { name: "sa_csrf", value: "stub-csrf", domain: "localhost", path: "/", expires: -1, httpOnly: false, secure: false, sameSite: "Lax" },
      ],
      origins: [],
    },
  },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } },
    { name: "mobile", use: { ...devices["Desktop Chrome"], viewport: { width: 390, height: 844 } } },
  ],
  // Never a running dev stack's servers: those may face the real backend, as an operator.
  webServer: [
    {
      command: "node --experimental-strip-types --disable-warning=ExperimentalWarning scripts/dev-stub.ts",
      cwd: "../..",
      url: `http://127.0.0.1:${STUB}/api/v1/me`,
      reuseExistingServer: false,
      env: { STUB_PORT: String(STUB), STUB_ROLE: "admin", STUB_LIVE: "off" }, // no live activity moving a frame
    },
    {
      command: `npx next dev --port ${WEB}`,
      cwd: "../..",
      url: `http://127.0.0.1:${WEB}/`,
      reuseExistingServer: false,
      timeout: 120_000,
      env: { PANEL_DEV_BACKEND: `http://127.0.0.1:${STUB}` },
    },
  ],
});
