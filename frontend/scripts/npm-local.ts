/**
 * Our own packages from their local checkouts, over what npm installed: `_.npm-local.json` (gitignored,
 * absent outside development) maps a package to its directory, e.g.
 *
 *   {"@evinvest/uikit": "/home/v/s/ev/web_std/ts/uikit"}
 *
 * Each is copied (its `package.json` and `files`, built), not linked: a link resolves `react` from
 * the checkout's own `node_modules`, a second React. Runs after every `npm install`/`npm ci`; run
 * `npm run local` again after rebuilding one.
 */
import { cpSync, existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";

const MAP = "_.npm-local.json";

if (existsSync(MAP)) {
  const local: Record<string, string> = JSON.parse(readFileSync(MAP, "utf8"));
  for (const [name, dir] of Object.entries(local)) {
    const manifest: { name: string; version: string; files?: string[] } = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    if (manifest.name !== name) throw new Error(`${MAP}: ${dir} is ${manifest.name}, not ${name}`);
    if (manifest.files === undefined) throw new Error(`${dir}/package.json: no \`files\`, so what it publishes is unknown`);
    const into = join("node_modules", name);
    rmSync(into, { recursive: true, force: true });
    cpSync(join(dir, "package.json"), join(into, "package.json"));
    for (const f of manifest.files) {
      if (!existsSync(join(dir, f))) throw new Error(`${name}: ${dir}/${f} is not built`);
      cpSync(join(dir, f), join(into, f), { recursive: true });
    }
    console.log(`npm-local: ${name}@${manifest.version} from ${dir}`);
  }
}
