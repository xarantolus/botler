//! Builds the web app (`frontend/dist`, compiled into the binary by `web.rs`) when it is missing
//! or was built from other sources than the checkout's. It is not committed; this needs Node.js
//! and npm (`npm ci` fetches the dependencies the first time).

use std::path::Path;
use std::process::Command;

fn main() {
    let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend");
    for f in [
        "src",
        "public",
        "index.html",
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "tsconfig.json",
        "dist/.source-hash",
    ] {
        println!("cargo::rerun-if-changed={}", frontend.join(f).display());
    }
    // `source-hash.mjs check` fails on a missing or stale dist.
    if npm(&frontend, "node", &["scripts/source-hash.mjs", "check"]).is_ok() {
        return;
    }
    if !frontend.join("node_modules").exists() {
        npm(&frontend, "npm", &["ci", "--no-audit", "--no-fund"]).unwrap_or_else(|e| fail(&e));
    }
    npm(&frontend, "npm", &["run", "build"]).unwrap_or_else(|e| fail(&e));
}

fn npm(dir: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("could not run {program} (install Node.js 22 or newer): {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} {} failed:\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

fn fail(e: &str) -> ! {
    panic!("building the web app (frontend/) failed: {e}");
}
