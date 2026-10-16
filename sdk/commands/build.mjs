// Builds dist/ for the custom-commands kit with Node's built-in TypeScript type
// stripping, so neither Bun nor tsc is needed to package Hello.
// Usage: node build.mjs [output-dir]
import { stripTypeScriptTypes } from "node:module"
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = dirname(fileURLToPath(import.meta.url))
const output = process.argv[2] ?? join(root, "dist")
rmSync(output, { recursive: true, force: true })
mkdirSync(output, { recursive: true })
for (const file of readdirSync(join(root, "src"))) {
  if (!file.endsWith(".ts")) continue
  const source = readFileSync(join(root, "src", file), "utf8")
  writeFileSync(join(output, file.replace(/\.ts$/, ".js")), stripTypeScriptTypes(source))
}
