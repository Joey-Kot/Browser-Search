#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const licensesRoot = resolve(root, "THIRD_PARTY_LICENSES");
const outputPath = resolve(root, "THIRD_PARTY_LICENSES.md");
const checkOnly = process.argv.includes("--check");

const reviewedSelections = new Map([
  ["Apache-2.0", "Apache-2.0"],
  ["Apache-2.0 AND ISC", "Apache-2.0 AND ISC"],
  ["Apache-2.0 OR BSL-1.0", "Apache-2.0"],
  ["Apache-2.0 OR MIT", "MIT"],
  ["Apache-2.0 OR ISC OR MIT", "MIT"],
  ["Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT", "MIT"],
  ["BSD-2-Clause OR Apache-2.0 OR MIT", "MIT"],
  ["BSD-3-Clause", "BSD-3-Clause"],
  ["(MIT OR Apache-2.0) AND Unicode-3.0", "MIT AND Unicode-3.0"],
  ["MIT", "MIT"],
  ["MIT AND BSD-3-Clause", "MIT AND BSD-3-Clause"],
  ["MIT OR Apache-2.0", "MIT"],
  ["MIT OR Apache-2.0 OR LGPL-2.1-or-later", "MIT"],
  ["MIT/Apache-2.0", "MIT"],
  ["CDLA-Permissive-2.0", "CDLA-Permissive-2.0"],
  ["ISC", "ISC"],
  [
    "ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)",
    "Apache-2.0 AND ISC AND MIT AND BSD-3-Clause"
  ],
  ["ISC AND (Apache-2.0 OR ISC)", "Apache-2.0 AND ISC"],
  ["Unicode-3.0", "Unicode-3.0"],
  ["Unlicense OR MIT", "MIT"]
]);

const requiredLicenseFiles = [
  "Apache-2.0.txt",
  "Atomic-Waker-THIRD-PARTY.txt",
  "AWS-LC-SYS-THIRD-PARTY.txt",
  "BSD-3-Clause.txt",
  "BSD-3-Clause-matchit.txt",
  "CDLA-Permissive-2.0.txt",
  "ISC.txt",
  "MIT.txt",
  "Spin-MIT.txt",
  "Unicode-3.0.txt",
  "Unicode-Data-Files.txt"
];

const additionalNotices = new Map([
  [
    "atomic-waker",
    {
      versions: new Set(["1.1.2"]),
      links: [
        "[embedded Tokio/futures notices](THIRD_PARTY_LICENSES/Atomic-Waker-THIRD-PARTY.txt)"
      ]
    }
  ],
  [
    "aws-lc-sys",
    {
      versions: new Set(["0.43.0"]),
      links: [
        "[AWS-LC third-party notices](THIRD_PARTY_LICENSES/AWS-LC-SYS-THIRD-PARTY.txt)"
      ]
    }
  ],
  [
    "regex-syntax",
    {
      versions: new Set(["0.8.11"]),
      links: [
        "[Unicode data notice](THIRD_PARTY_LICENSES/Unicode-Data-Files.txt)"
      ]
    }
  ],
  [
    "tracing-core",
    {
      versions: new Set(["0.1.36"]),
      links: ["[embedded spin code notice](THIRD_PARTY_LICENSES/Spin-MIT.txt)"]
    }
  ]
]);

function compareStrings(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024
  })
);

const workspacePackages = new Set(
  metadata.workspace_members.map((id) => {
    const packageMetadata = metadata.packages.find(
      (candidate) => candidate.id === id
    );
    if (!packageMetadata) {
      throw new Error(`Workspace package is missing from Cargo metadata: ${id}`);
    }
    return `${packageMetadata.name}@${packageMetadata.version}`;
  })
);

const packageDetails = new Map();
for (const packageMetadata of metadata.packages) {
  const key = `${packageMetadata.name}@${packageMetadata.version}`;
  const existing = packageDetails.get(key);
  if (existing && existing.id !== packageMetadata.id) {
    throw new Error(
      `Multiple Cargo sources use ${key}; update the generator to distinguish them`
    );
  }
  packageDetails.set(key, packageMetadata);
}

const tree = execFileSync(
  "cargo",
  [
    "tree",
    "--quiet",
    "--locked",
    "--edges",
    "normal,no-proc-macro",
    "--target",
    "all",
    "--prefix",
    "none",
    "--format",
    "{p}"
  ],
  {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024
  }
);

const dependenciesByKey = new Map();
for (const rawLine of tree.split("\n")) {
  const packageDisplay = rawLine.replace(" (*)", "").trim();
  if (!packageDisplay) {
    continue;
  }

  const packageMatch = packageDisplay.match(/^(\S+) v(\S+)/);
  if (!packageMatch) {
    throw new Error(`Unexpected Cargo package display: ${packageDisplay}`);
  }

  const [, name, version] = packageMatch;
  const key = `${name}@${version}`;
  if (workspacePackages.has(key)) {
    continue;
  }

  const details = packageDetails.get(key);
  if (!details) {
    throw new Error(`Cargo metadata is missing dependency details for ${key}`);
  }
  dependenciesByKey.set(key, details);
}

const dependencies = [...dependenciesByKey.values()].sort(
  (left, right) =>
    compareStrings(left.name, right.name) ||
    compareStrings(left.version, right.version)
);

function selectedLicense(expression) {
  const selection = reviewedSelections.get(expression);
  if (!selection) {
    throw new Error(
      `No reviewed license selection for exact SPDX expression: ${expression}`
    );
  }
  return selection;
}

function baseLicenseLinks(packageMetadata, selection) {
  if (selection === "MIT") {
    return ["[MIT](THIRD_PARTY_LICENSES/MIT.txt)"];
  }
  if (selection === "Apache-2.0") {
    return ["[Apache-2.0](THIRD_PARTY_LICENSES/Apache-2.0.txt)"];
  }
  if (selection === "Apache-2.0 AND ISC") {
    return [
      "[Apache-2.0](THIRD_PARTY_LICENSES/Apache-2.0.txt)",
      "[ISC](THIRD_PARTY_LICENSES/ISC.txt)"
    ];
  }
  if (selection === "Apache-2.0 AND ISC AND MIT AND BSD-3-Clause") {
    return [
      "[Apache-2.0](THIRD_PARTY_LICENSES/Apache-2.0.txt)",
      "[ISC](THIRD_PARTY_LICENSES/ISC.txt)",
      "[MIT](THIRD_PARTY_LICENSES/MIT.txt)",
      "[BSD-3-Clause](THIRD_PARTY_LICENSES/BSD-3-Clause.txt)"
    ];
  }
  if (selection === "BSD-3-Clause") {
    return ["[BSD-3-Clause](THIRD_PARTY_LICENSES/BSD-3-Clause.txt)"];
  }
  if (selection === "CDLA-Permissive-2.0") {
    return [
      "[CDLA-Permissive-2.0](THIRD_PARTY_LICENSES/CDLA-Permissive-2.0.txt)"
    ];
  }
  if (selection === "ISC") {
    return ["[ISC](THIRD_PARTY_LICENSES/ISC.txt)"];
  }
  if (selection === "Unicode-3.0") {
    return ["[Unicode-3.0](THIRD_PARTY_LICENSES/Unicode-3.0.txt)"];
  }
  if (
    selection === "MIT AND Unicode-3.0" &&
    packageMetadata.name === "unicode-ident"
  ) {
    return [
      "[MIT](THIRD_PARTY_LICENSES/MIT.txt)",
      "[Unicode-3.0](THIRD_PARTY_LICENSES/Unicode-3.0.txt)"
    ];
  }
  if (
    selection === "MIT AND BSD-3-Clause" &&
    packageMetadata.name === "matchit"
  ) {
    return [
      "[MIT](THIRD_PARTY_LICENSES/MIT.txt)",
      "[BSD-3-Clause](THIRD_PARTY_LICENSES/BSD-3-Clause-matchit.txt)"
    ];
  }
  throw new Error(
    `No reviewed license files for ${packageMetadata.name} ${packageMetadata.version}: ${selection}`
  );
}

function licenseLinks(packageMetadata, selection) {
  const links = baseLicenseLinks(packageMetadata, selection);
  const additional = additionalNotices.get(packageMetadata.name);
  if (!additional) {
    return links;
  }
  if (!additional.versions.has(packageMetadata.version)) {
    throw new Error(
      `Additional notices for ${packageMetadata.name} ${packageMetadata.version} have not been reviewed`
    );
  }
  return [...links, ...additional.links];
}

function escapeTable(value) {
  return value.replaceAll("|", "\\|").replaceAll("\n", " ");
}

const rows = dependencies.map((packageMetadata) => {
  if (!packageMetadata.license) {
    throw new Error(
      `Dependency ${packageMetadata.name} ${packageMetadata.version} has no declared license`
    );
  }

  const selection = selectedLicense(packageMetadata.license);
  const crateUrl = `https://crates.io/crates/${packageMetadata.name}/${packageMetadata.version}`;
  const upstream = packageMetadata.repository || packageMetadata.homepage;
  const sourceLinks = upstream
    ? `[crates.io](${crateUrl}), [upstream](${upstream})`
    : `[crates.io](${crateUrl})`;
  const authors =
    packageMetadata.authors.length > 0
      ? packageMetadata.authors.join("; ")
      : "See upstream source";
  const notices = licenseLinks(packageMetadata, selection).join(", ");

  return `| \`${packageMetadata.name}\` \`${packageMetadata.version}\` | \`${escapeTable(packageMetadata.license)}\` | \`${selection}\` | ${notices} | ${escapeTable(authors)} | ${sourceLinks} |`;
});

const output = `# Third-Party Licenses

Browser Search is licensed under GPL-3.0-or-later. The components listed below are third-party works and remain available under their respective licenses.

This inventory is generated from the locked Cargo dependency graph. It covers normal dependencies reachable for any supported target and excludes development dependencies, build-only dependencies, and procedural macro tooling that is not distributed with the executable.

Standard license texts are stored once rather than duplicated for every crate. Package-specific files are retained only when they contain an additional copyright, attribution, embedded-code, or data-license notice that the standard text does not cover. Upstream source archives remain the authoritative record.

The Chrome extension's npm packages are development-only build tools and are not included in \`extension/dist/\`; they are therefore outside the scope of the runtime distribution inventory below.

## Included license and notice texts

- [MIT](THIRD_PARTY_LICENSES/MIT.txt)
- [Apache License 2.0](THIRD_PARTY_LICENSES/Apache-2.0.txt)
- [ISC License](THIRD_PARTY_LICENSES/ISC.txt)
- [BSD 3-Clause License](THIRD_PARTY_LICENSES/BSD-3-Clause.txt)
- [Community Data License Agreement - Permissive 2.0](THIRD_PARTY_LICENSES/CDLA-Permissive-2.0.txt)
- [AWS-LC bundled third-party notices](THIRD_PARTY_LICENSES/AWS-LC-SYS-THIRD-PARTY.txt)
- [matchit / httprouter BSD 3-Clause notice](THIRD_PARTY_LICENSES/BSD-3-Clause-matchit.txt)
- [Unicode License v3](THIRD_PARTY_LICENSES/Unicode-3.0.txt)
- [Unicode data files notice used by regex-syntax](THIRD_PARTY_LICENSES/Unicode-Data-Files.txt)
- [atomic-waker embedded Tokio/futures notices](THIRD_PARTY_LICENSES/Atomic-Waker-THIRD-PARTY.txt)
- [tracing-core embedded spin code notice](THIRD_PARTY_LICENSES/Spin-MIT.txt)

## License selection

Where an upstream component is offered under multiple licenses, this distribution uses the MIT option when available. Components without an MIT option use the license terms shown below. An \`AND\` expression means every listed license applies.

## Components

| Component | Declared SPDX expression | License used | Included text or notice | Upstream authors | Source |
|---|---|---|---|---|---|
${rows.join("\n")}
`;

function listFiles(directory, prefix = "") {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const relativePath = prefix ? `${prefix}/${entry.name}` : entry.name;
    const absolutePath = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...listFiles(absolutePath, relativePath));
    } else if (entry.isFile()) {
      files.push(relativePath);
    }
  }
  return files.sort(compareStrings);
}

for (const filename of requiredLicenseFiles) {
  if (!existsSync(resolve(licensesRoot, filename))) {
    throw new Error(`Required third-party notice is missing: ${filename}`);
  }
}

const actualLicenseFiles = listFiles(licensesRoot);
if (
  JSON.stringify(actualLicenseFiles) !==
  JSON.stringify([...requiredLicenseFiles].sort(compareStrings))
) {
  throw new Error(
    "THIRD_PARTY_LICENSES contains missing or unexpected files; keep only the reviewed texts used by this project"
  );
}

if (checkOnly) {
  let existing;
  try {
    existing = readFileSync(outputPath, "utf8");
  } catch {
    throw new Error(
      "THIRD_PARTY_LICENSES.md is missing; regenerate it before packaging"
    );
  }

  if (existing !== output) {
    throw new Error(
      "THIRD_PARTY_LICENSES.md is stale; run node scripts/generate-third-party-licenses.mjs"
    );
  }
} else {
  writeFileSync(outputPath, output);
}
