// Copies the version changesets wrote to package.json into the Cargo
// workspace (Cargo.toml and Cargo.lock), so the binary reports it too.
import { readFileSync, writeFileSync } from "node:fs";

const { version } = JSON.parse(readFileSync("package.json", "utf8"));

const manifest = readFileSync("Cargo.toml", "utf8");
const bumped = manifest.replace(
  /(\[workspace\.package\][^[]*?\nversion = )"[^"]*"/,
  `$1"${version}"`,
);
if (bumped === manifest && !manifest.includes(`version = "${version}"`)) {
  throw new Error("Cargo.toml: no [workspace.package] version to update");
}
writeFileSync("Cargo.toml", bumped);

const lock = readFileSync("Cargo.lock", "utf8");
writeFileSync(
  "Cargo.lock",
  lock.replace(/(name = "localflake(?:-[a-z]+)?"\nversion = )"[^"]*"/g, `$1"${version}"`),
);
console.log(`Cargo workspace version set to ${version}`);
