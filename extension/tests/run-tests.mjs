import { build } from "esbuild";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
for (const testFile of ["google.test.ts", "jobs.test.ts", "cdp.test.ts"]) {
  const result = await build({
    entryPoints: [resolve(root, testFile)],
    bundle: true,
    platform: "node",
    format: "esm",
    target: "node24",
    write: false
  });

  const code = result.outputFiles[0]?.text;
  if (!code) {
    throw new Error(`Test bundle is empty: ${testFile}`);
  }
  await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);
}
console.log("extension tests passed");
