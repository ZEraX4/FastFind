// Builds the website into _site/: the landing page from site/ plus the UI's in-browser demo
// (sample data, no backend) in _site/demo/. Used by .github/workflows/pages.yml.
// Usage: npm run site:build
import { cpSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { build } from "vite";

const out = resolve("_site");
// Empty rather than delete the folder: a local preview server may be serving from it.
mkdirSync(out, { recursive: true });
for (const entry of readdirSync(out)) rmSync(resolve(out, entry), { recursive: true, force: true });
cpSync("site", out, { recursive: true });
cpSync("ui/assets/icon.svg", resolve(out, "icon.svg"));

// The hosted demo says it searches sample files (see ui/src/demo.ts).
process.env.VITE_SITE_DEMO = "1";
await build({
  configFile: resolve("vite.config.ts"),
  base: "./",
  logLevel: "warn",
  build: { outDir: resolve(out, "demo"), emptyOutDir: true },
});
console.log(`site built in ${out}`);
