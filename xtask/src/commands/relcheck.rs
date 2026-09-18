use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::from_utf8,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum as _};
use semver::{Comparator, Op, Version, VersionReq};
use strum::IntoEnumIterator;
use toml_edit::Table;

use crate::{Package, cargo::CargoToml, windows_safe_path};

#[derive(Debug, Subcommand)]
pub enum RelCheckCmds {
    /// Initialize the local registry
    Init,

    /// Deinitialize the local registry
    Deinit,

    /// Package the release plan's crates into the local registry at their
    /// planned versions.
    Update(PlanSelector),

    /// Rewrite `esp-*` path dependencies in examples and tests to the versions
    /// available in the local registry.
    ReplacePathDeps(PlanSelector),
    /// Validate workspace esp-rom-sys dependency version policy.
    CheckRomSysPolicy,
}

/// Selects the release plan a command operates on: either a local file
/// (default `release_plan.jsonc`) or the plan embedded in a release PR body.
#[derive(Args, Debug, Clone)]
pub struct PlanSelector {
    /// Path to a finalized release plan file.
    #[arg(
        long,
        default_value = "release_plan.jsonc",
        conflicts_with = "plan_from_pr"
    )]
    plan: PathBuf,

    /// Read the plan from the body of this release PR instead of a local file.
    #[arg(long)]
    plan_from_pr: Option<u64>,
}

pub fn run_rel_check(args: RelCheckCmds) -> Result<()> {
    ensure_cargo_local_registry()?;

    match args {
        RelCheckCmds::Init => init_rel_check()?,
        RelCheckCmds::Deinit => deinit_rel_check()?,
        RelCheckCmds::Update(selector) => {
            update(&load_plan(Some(selector.plan), selector.plan_from_pr)?)?
        }
        RelCheckCmds::ReplacePathDeps(selector) => {
            scrap_path_deps(&load_plan(Some(selector.plan), selector.plan_from_pr)?)?
        }
        RelCheckCmds::CheckRomSysPolicy => check_rom_sys_policy(Path::new("."))?,
    }

    Ok(())
}

/// Minimal view of `release_plan.jsonc`.
///
/// The registry check runs without the `release` feature, so it cannot borrow
/// the full `Plan` type from the release code. Only the fields this check reads
/// are modelled; every other field in the plan is ignored.
#[derive(Debug, Clone, serde::Deserialize)]
struct PlanFile {
    packages: Vec<PlanEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct PlanEntry {
    package: Package,
    new_version: Version,
}

/// Load the plan from a release PR body or a local file. A PR number wins over
/// the `--plan` path, which always carries its default, so `--plan-from-pr`
/// takes effect without the caller clearing the default.
fn load_plan(plan: Option<PathBuf>, plan_from_pr: Option<u64>) -> Result<PlanFile> {
    if let Some(pr) = plan_from_pr {
        let body = fetch_pr_body(pr)?;
        return parse_plan(&body)
            .with_context(|| format!("Failed to parse the release plan embedded in PR #{pr}"));
    }

    let path = plan.unwrap_or_else(|| PathBuf::from("release_plan.jsonc"));
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read release plan from {}", path.display()))?;
    parse_plan(&source)
        .with_context(|| format!("Failed to parse release plan from {}", path.display()))
}

/// Fetch a PR body through the GitHub CLI so the embedded plan can be read.
fn fetch_pr_body(pr: u64) -> Result<String> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.to_string(),
            "--repo",
            crate::UPSTREAM_REPO,
            "--json",
            "body",
            "-q",
            ".body",
        ])
        .output()
        .context("Failed to run `gh pr view`. Is the GitHub CLI installed and authenticated?")?;

    if !output.status.success() {
        bail!(
            "`gh pr view {pr}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse a [`PlanFile`] from either the raw `release_plan.jsonc` contents
/// (which may still carry the `//` header the planner writes) or a PR body that
/// embeds the plan in a fenced `jsonc` block.
fn parse_plan(source: &str) -> Result<PlanFile> {
    let json = extract_fenced_jsonc(source).unwrap_or_else(|| source.to_string());

    // Drop the `//` header a freshly generated plan carries; it is not valid
    // JSON. A finalized plan (and a fenced PR block) has no such lines, so this
    // is a no-op there.
    let json = json
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    serde_json::from_str::<PlanFile>(&json).context("Release plan is not valid JSON")
}

/// Return the contents of the first fenced `jsonc` block in `body`, if any.
fn extract_fenced_jsonc(body: &str) -> Option<String> {
    let mut collecting = false;
    let mut collected = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if collecting {
            if trimmed == "```" {
                return Some(collected.join("\n"));
            }
            collected.push(line);
        } else if trimmed.starts_with("```jsonc") {
            collecting = true;
        }
    }
    None
}

/// The crates to package from the working tree: exactly the plan's crates.
///
/// Every other published crate must already be in the registry from crates.io
/// (via `init`); packaging one from the working tree would risk publishing
/// unreleased content under an already-released version number.
fn packages_to_package_from_tree(plan: &PlanFile) -> Vec<Package> {
    plan.packages.iter().map(|entry| entry.package).collect()
}

/// Pick the version an `esp-*` path dependency is rewritten to: the plan's
/// `new_version` when the crate is being released, otherwise the newest version
/// already in the local registry (synced from crates.io). A frozen crate must
/// never resolve to a working-tree version.
fn choose_replacement_version(
    krate: &Package,
    plan: &PlanFile,
    registry_versions: &[Version],
) -> Result<Version> {
    if let Some(entry) = plan.packages.iter().find(|entry| &entry.package == krate) {
        return Ok(entry.new_version.clone());
    }

    registry_versions.iter().max().cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "{krate} is not in the release plan and has no version in the local registry. \
             Frozen crates are synced from crates.io by `init` and must be present here."
        )
    })
}

/// Environment for cargo subprocesses with the outer `CARGO*` variables
/// stripped, so a nested `cargo run` (the xtask alias) does not leak its own
/// resolver configuration into the invoked cargo.
fn cargo_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| !k.starts_with("CARGO"))
        .collect()
}

/// Outcome of a cargo invocation whose stderr was captured.
struct CargoOutcome {
    success: bool,
    stderr: String,
}

/// Run a cargo invocation, capturing stderr. Errors only if the process cannot
/// be spawned; a non-zero exit is reported in the outcome so the caller can
/// clean up before deciding how to fail.
fn run_cargo(cmd: &mut std::process::Command) -> Result<CargoOutcome> {
    let output = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .context("Failed to spawn cargo")?;

    Ok(CargoOutcome {
        success: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run a cargo step whose non-resolution failures are tolerated (matching the
/// registry's best-effort priming), but whose resolution failures must not be
/// swallowed or retried into a confusing downstream error.
fn run_cargo_lenient(cmd: &mut std::process::Command, what: &str) -> Result<()> {
    let outcome = run_cargo(cmd)?;
    if !outcome.success {
        if is_resolution_failure(&outcome.stderr) {
            return Err(cargo_failure(what, &outcome));
        }
        log::warn!(
            "cargo step failed while {what} (continuing):\n{}",
            outcome.stderr
        );
    }
    Ok(())
}

/// Whether cargo's stderr names a dependency-resolution failure. The message
/// already lists the crate, the requirement, and the versions available, so the
/// caller can surface it verbatim.
fn is_resolution_failure(stderr: &str) -> bool {
    const MARKERS: [&str; 4] = [
        "failed to select a version",
        "no matching package",
        "unable to find",
        "cannot find package",
    ];
    MARKERS.iter().any(|marker| stderr.contains(marker))
}

/// Build a single error for a failed cargo invocation, surfacing a resolution
/// failure verbatim so the registry check reports one clear cause.
fn cargo_failure(what: &str, outcome: &CargoOutcome) -> anyhow::Error {
    if is_resolution_failure(&outcome.stderr) {
        anyhow::anyhow!(
            "Dependency resolution failed while {what}. The local registry has no version that \
             satisfies a requirement:\n{}",
            outcome.stderr
        )
    } else {
        anyhow::anyhow!("cargo failed while {what}:\n{}", outcome.stderr)
    }
}

fn toolchain() -> String {
    "esp".to_string()
}

fn toolchain_folder(toolchain: &str) -> Result<PathBuf> {
    let toolchains = std::process::Command::new("rustup")
        .arg("toolchain")
        .arg("list")
        .arg("-v")
        .output()?;

    if !toolchains.status.success() {
        return Err(anyhow::anyhow!(
            "Unable to run rustup to learn about toolchains."
        ));
    }

    let toolchains = from_utf8(&toolchains.stdout)?.lines();
    let mut parsed: Vec<(String, String)> = Vec::new();
    let re = regex::Regex::new(r#"^(\S+)(?:\s+\([^)]*\))?\s+(\S.*)$"#)?;
    for line in toolchains {
        if let Some(parts) = re.captures(line) {
            let (name, folder) = if parts.len() == 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else {
                (parts[1].to_string(), parts[3].to_string())
            };

            let name = if name.starts_with("stable") {
                "stable".to_string()
            } else if name.starts_with("nightly") {
                if regex::Regex::new(r#"nightly-\d\d\d\d-\d\d-\d\d.*"#)
                    .unwrap()
                    .is_match(&name)
                {
                    name[..18].to_string()
                } else {
                    "nightly".to_string()
                }
            } else if regex::Regex::new(r#"\d+\.\d+-.*"#).unwrap().is_match(&name) {
                let separator_idx = name.chars().position(|c| c == '-').unwrap();
                name[..(separator_idx)].to_string()
            } else {
                name
            };

            parsed.push((name, folder));
        };
    }

    match parsed.iter().find(|(name, _)| name == toolchain) {
        Some((_, folder)) => Ok(PathBuf::from(folder)),
        None => Err(anyhow::anyhow!(
            "Toolchain {toolchain} not found. Found {:?}",
            parsed
        )),
    }
}

fn deinit_rel_check() -> Result<()> {
    let _ = std::fs::remove_dir_all("compile-tests/.cargo");

    if std::fs::exists("target/local-registry")? {
        std::fs::remove_dir_all("target/local-registry")?;
    }
    revert_scrap_path_deps()?;

    Ok(())
}

fn init_rel_check() -> Result<()> {
    fn prepare_for_directory(project: &Path) -> Result<()> {
        log::info!("Processing {}", project.display());

        let _ = std::fs::remove_file(project.join("Cargo.lock"));

        // make sure we have the `Cargo.lock` file
        let status = std::process::Command::new("cargo")
            .arg(format!("+{}", toolchain()))
            .arg("metadata")
            .arg("--format-version=1")
            .current_dir(&project)
            .stdout(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            log::warn!("Failed");
        }

        // add all dependencies referenced by the lock file
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("local-registry");
        cmd.arg("--no-delete");
        cmd.arg("--sync");
        cmd.arg("Cargo.lock");
        cmd.arg(windows_safe_path(
            &std::path::PathBuf::from("target/local-registry")
                .canonicalize()
                .unwrap(),
        ));
        cmd.stdout(std::process::Stdio::null());
        cmd.env_clear();
        cmd.envs(
            std::env::vars()
                .into_iter()
                .filter(|(k, _)| !k.starts_with("CARGO")),
        );
        cmd.current_dir(&project);

        log::info!("{:?}", cmd);
        cmd.status()?;
        Ok(())
    }

    // cleanup
    if std::fs::exists("target/local-registry")? {
        std::fs::remove_dir_all("target/local-registry")?;
    }
    std::fs::create_dir("target/local-registry")?;

    // prepare latest released versions
    prepare_for_directory(Path::new("init-local-registry"))?;

    // prepare versions from compile-tests - might not be latest released
    for dir in std::fs::read_dir("compile-tests")? {
        if let Ok(dir) = dir {
            if dir.file_type()?.is_dir() {
                prepare_for_directory(&dir.path())?;
            }
        }
    }

    // install dependencies needed for build-std
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("local-registry");
    cmd.arg("--no-delete");
    cmd.arg("--sync");
    cmd.arg(format!(
        "{}/lib/rustlib/src/rust/library/Cargo.lock",
        toolchain_folder(&toolchain())?.display()
    ));
    cmd.arg("target/local-registry");
    cmd.stdout(std::process::Stdio::null());
    cmd.env_clear();
    cmd.envs(
        std::env::vars()
            .into_iter()
            .filter(|(k, _)| !k.starts_with("CARGO")),
    );

    log::info!("{:?}", cmd);
    cmd.status()?;

    // "for reasons" (e.g. running examples later via the "normal" xtask) also do it for other
    // toolchains
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("local-registry");
    cmd.arg("--no-delete");
    cmd.arg("--sync");
    cmd.arg(format!(
        "{}/lib/rustlib/src/rust/library/Cargo.lock",
        toolchain_folder("nightly")?.display()
    ));
    cmd.arg("target/local-registry");
    cmd.stdout(std::process::Stdio::null());
    cmd.env_clear();
    cmd.envs(
        std::env::vars()
            .into_iter()
            .filter(|(k, _)| !k.starts_with("CARGO")),
    );

    log::info!("{:?}", cmd);
    cmd.status()?;

    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("local-registry");
    cmd.arg("--no-delete");
    cmd.arg("--sync");
    cmd.arg(format!(
        "{}/lib/rustlib/src/rust/library/Cargo.lock",
        toolchain_folder("stable")?.display()
    ));
    cmd.arg("target/local-registry");
    cmd.stdout(std::process::Stdio::null());
    cmd.env_clear();
    cmd.envs(
        std::env::vars()
            .into_iter()
            .filter(|(k, _)| !k.starts_with("CARGO")),
    );

    log::info!("{:?}", cmd);
    cmd.status()?;

    // and lastly do it for the xtask itself
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("local-registry");
    cmd.arg("--no-delete");
    cmd.arg("--sync");
    cmd.arg("Cargo.lock");
    cmd.arg(windows_safe_path(
        &std::path::PathBuf::from("target/local-registry")
            .canonicalize()
            .unwrap(),
    ));
    cmd.stdout(std::process::Stdio::null());
    cmd.env_clear();
    cmd.envs(
        std::env::vars()
            .into_iter()
            .filter(|(k, _)| !k.starts_with("CARGO")),
    );

    log::info!("{:?}", cmd);
    cmd.status()?;

    Ok(())
}

fn update(plan: &PlanFile) -> Result<()> {
    if !std::fs::exists("target/local-registry")? {
        bail!("Cannot update - run `init` first.");
    }

    let workspace = Path::new(".");

    let plan_versions = plan
        .packages
        .iter()
        .map(|entry| (entry.package, entry.new_version.clone()))
        .collect::<HashMap<_, _>>();

    let mut packages_to_release = packages_to_package_from_tree(plan);
    packages_to_release.sort();
    packages_to_release.dedup();

    let mut package_tomls = packages_to_release
        .iter()
        .map(|pkg| CargoToml::new(workspace, *pkg).map(|cargo_toml| (*pkg, cargo_toml)))
        .collect::<Result<HashMap<_, _>>>()?;

    // Restrict the dependency graph to plan crates so each is packaged after
    // the plan crates it depends on: their `.crate` files must already be in
    // the registry to resolve. Dependencies outside the plan come from
    // crates.io and are already present.
    let mut dep_graph = HashMap::new();
    for (package, toml) in package_tomls.iter_mut() {
        let deps = toml
            .repo_dependencies()
            .into_iter()
            .filter(|dep| plan_versions.contains_key(dep))
            .collect();
        dep_graph.insert(*package, deps);
    }

    // Topological sort the packages into a release order. Note that this is not a stable order,
    // because the source data is a HashMap which does not have a stable insertion order. This is
    // okay, as long as the relationships of the dependencies are kept.
    let sorted = topological_sort(&dep_graph);
    log::info!("Sorted packages: {:?}", sorted);

    let original_config = std::fs::read_to_string(".cargo/config.toml")?;

    for package in sorted.iter() {
        log::info!("Package = {}", package);

        let planned_version = plan_versions
            .get(package)
            .expect("topological sort only contains plan crates");

        let toml_ref = package.toml();
        let Some(ref toml) = *toml_ref else {
            bail!("Cannot package {package}: no Cargo.toml found");
        };
        let package_path = toml.package_path();
        let version_str = toml.version().to_string();
        let version = toml.package_version();

        // The release branch working tree carries the bumped versions, so
        // packaging the tree must yield exactly the planned version. A mismatch
        // means the tree and the plan disagree; refuse rather than publish a
        // wrong version into the registry.
        ensure!(
            &version == planned_version,
            "Working-tree version of {package} is {version}, but the plan releases \
             {planned_version}. The release branch must carry the planned versions."
        );

        if std::fs::exists(format!(
            "target/local-registry/{package}-{version_str}.crate"
        ))? {
            log::warn!("Already exists as version {version_str}");
            continue;
        }

        core::mem::drop(toml_ref);
        package.remove_toml_from_cache();

        log::info!("Updating...");

        // make sure we have a lock file
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("update")
            .current_dir(&package_path)
            .env_clear()
            .envs(cargo_env());
        run_cargo_lenient(&mut cmd, &format!("running `cargo update` for {package}"))?;

        // prepare all the deps we need
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("local-registry")
            .arg("--no-delete")
            .arg("--sync")
            .arg("Cargo.lock")
            .arg("../target/local-registry")
            .current_dir(&package_path)
            .env_clear()
            .envs(cargo_env());
        run_cargo_lenient(&mut cmd, &format!("syncing dependencies for {package}"))?;

        std::fs::write(
            ".cargo/config.toml",
            format!(
                r#"{}
        # {}{}
        [source.crates-io]
        registry = 'sparse+https://index.crates.io/'
        replace-with = 'local-registry'

        [source.local-registry]
        local-registry = '{}'
    "#,
                &original_config,
                "STOP",
                "SHIP",
                windows_safe_path(
                    &std::path::PathBuf::from("target/local-registry")
                        .canonicalize()
                        .unwrap()
                )
                .display()
            ),
        )?;
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("package")
            .arg("--no-verify")
            .arg("--verbose")
            .arg("--allow-dirty")
            .arg("--index=http://crates.io")
            .arg("--target-dir=../target")
            .current_dir(&package_path)
            .env_clear()
            .envs(cargo_env());
        let outcome = run_cargo(&mut cmd)?;

        // Restore the workspace config before reacting to the result so a
        // failure does not leave the source-replacement block behind.
        std::fs::write(".cargo/config.toml", &original_config)?;

        if !outcome.success {
            // A plan crate that fails to package is a hard error: the registry
            // would otherwise be missing a crate this release needs.
            return Err(cargo_failure(&format!("packaging {package}"), &outcome));
        }

        // copy the crate to our registry
        let toml = package.toml();
        let Some(ref toml) = *toml else {
            unreachable!("");
        };
        let krate = toml.manifest["package"]["name"].as_str().unwrap();
        let version = toml.manifest["package"]["version"].as_str().unwrap();
        std::fs::copy(
            PathBuf::from(".")
                .join("target")
                .join("package")
                .join(format!("{}-{}.crate", krate, version)),
            PathBuf::from(format!("target/local-registry/{}-{}.crate", krate, version)),
        )?;

        // copy metadata to the index
        let index_file = PathBuf::from("target/local-registry/index/")
            .join(&krate[..2])
            .join(&krate[2..][..2])
            .join(krate);

        if std::fs::exists(&index_file)? {
            let index_file_contents = std::fs::read_to_string(&index_file)?;
            let index_file_contents = index_file_contents.lines();
            let mut entries = Vec::new();
            for line in index_file_contents {
                let item: serde_json::Value = serde_json::de::from_str(line).unwrap();
                if item["vers"] != version {
                    entries.push(item);
                }
            }
            entries.push(
                serde_json::de::from_str(
                    &std::fs::read_to_string(
                        PathBuf::from(".")
                            .join("target")
                            .join("package")
                            .join("tmp-registry")
                            .join("index")
                            .join(&krate[..2])
                            .join(&krate[2..][..2])
                            .join(krate),
                    )
                    .unwrap(),
                )
                .unwrap(),
            );
            let mut contents = String::new();
            for entry in entries {
                contents.push_str(&serde_json::to_string(&entry).unwrap());
                contents.push('\n');
            }
            std::fs::write(&index_file, contents)?;
        } else {
            // just copy
            std::fs::copy(
                PathBuf::from(".")
                    .join("target")
                    .join("package")
                    .join("tmp-registry")
                    .join("index")
                    .join(&krate[..2])
                    .join(&krate[2..][..2])
                    .join(krate),
                &index_file,
            )?;
        }
    }

    Ok(())
}

/// Every version of `krate` present as a `.crate` file in the local registry.
fn registry_versions_of_crate(krate: &str) -> Result<Vec<Version>> {
    let prefix = format!("{krate}-");
    let mut versions = Vec::new();
    for entry in std::fs::read_dir("target/local-registry/")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // `esp-hal-embassy-1.0.0.crate` also starts with `esp-hal-`; the version
        // parse below rejects the leftover crate-name segment, so only exact
        // matches survive.
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(vstr) = rest.strip_suffix(".crate") else {
            continue;
        };
        if let Ok(v) = vstr.parse::<Version>() {
            versions.push(v);
        }
    }
    Ok(versions)
}

fn ensure_cargo_local_registry() -> Result<()> {
    let check = std::process::Command::new("cargo")
        .arg("local-registry")
        .arg("--help")
        .output();

    match check {
        Ok(out) => {
            if !out.status.success() {
                std::process::Command::new("cargo")
                    .arg("install")
                    .arg("cargo-local-registry")
                    .output()?;
            }
        }
        Err(_) => {
            std::process::Command::new("cargo")
                .arg("install")
                .arg("cargo-local-registry")
                .output()?;
        }
    }

    Ok(())
}

fn revert_scrap_path_deps() -> Result<()> {
    let pkgs = [
        crate::Package::Examples.to_string(),
        crate::Package::HilTest.to_string(),
        crate::Package::HilTestRadio.to_string(),
        crate::Package::QaTest.directory().to_string(),
    ];

    for pkg in pkgs {
        if let Ok(manifest_paths) = crate::find_packages(Path::new(&pkg)) {
            let manifest_paths = if !manifest_paths.is_empty() {
                manifest_paths
            } else {
                vec![PathBuf::from(&pkg)]
            };

            for manifest_path in manifest_paths {
                if std::fs::exists(manifest_path.join("Cargo.toml$"))? {
                    std::fs::remove_file(manifest_path.join("Cargo.toml"))?;
                    std::fs::rename(
                        manifest_path.join("Cargo.toml$"),
                        manifest_path.join("Cargo.toml"),
                    )?;
                }

                if std::fs::exists(manifest_path.join(".cargo/config.toml$"))? {
                    std::fs::remove_file(manifest_path.join(".cargo/config.toml"))?;
                    std::fs::rename(
                        manifest_path.join(".cargo/config.toml$"),
                        manifest_path.join(".cargo/config.toml"),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn scrap_path_deps(plan: &PlanFile) -> Result<()> {
    if !std::fs::exists("target/local-registry")? {
        bail!("Cannot scrap path dependencies - run `init` first.");
    }

    let pkgs = [
        crate::Package::Examples.to_string(),
        crate::Package::HilTest.to_string(),
        crate::Package::HilTestRadio.to_string(),
        crate::Package::QaTest.directory().to_string(),
    ];

    for pkg in pkgs {
        if let Ok(manifest_paths) = crate::find_packages(Path::new(&pkg)) {
            let manifest_paths = if !manifest_paths.is_empty() {
                manifest_paths
            } else {
                vec![PathBuf::from(&pkg)]
            };

            for manifest_path in manifest_paths {
                // make sure we have a lock file
                std::fs::remove_file(manifest_path.join("Cargo.lock")).ok();
                let mut cmd = std::process::Command::new("cargo");
                cmd.arg(format!("+{}", toolchain()))
                    .arg("metadata")
                    .arg("--format-version=1")
                    .current_dir(&manifest_path);
                run_cargo_lenient(
                    &mut cmd,
                    &format!("resolving metadata in {}", manifest_path.display()),
                )?;

                // add dependencies to the local registry
                let mut cmd = std::process::Command::new("cargo");
                cmd.arg("local-registry");
                cmd.arg("--no-delete");
                cmd.arg("--sync");
                cmd.arg("Cargo.lock");
                cmd.arg(windows_safe_path(
                    &std::path::PathBuf::from("target/local-registry")
                        .canonicalize()
                        .unwrap(),
                ));
                cmd.env_clear();
                cmd.envs(cargo_env());
                cmd.current_dir(&manifest_path);

                log::info!("{:?}", cmd);
                run_cargo_lenient(
                    &mut cmd,
                    &format!("syncing dependencies in {}", manifest_path.display()),
                )?;

                // rename files we are going to change
                std::fs::rename(
                    manifest_path.join("Cargo.toml"),
                    manifest_path.join("Cargo.toml$"),
                )?;
                if std::fs::exists(manifest_path.join(".cargo/config.toml"))? {
                    std::fs::rename(
                        manifest_path.join(".cargo/config.toml"),
                        manifest_path.join(".cargo/config.toml$"),
                    )?;
                }

                // scrap the path dependencies, use version from local registry
                let contents = std::fs::read_to_string(manifest_path.join("Cargo.toml$"))?;
                let mut toml = contents.parse::<toml_edit::DocumentMut>()?;

                for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
                    if toml.contains_key(key) {
                        for dep in toml[key].as_table_mut().unwrap().iter_mut() {
                            let krate = dep.0.get();

                            if krate.starts_with("esp-") {
                                let registry_versions = registry_versions_of_crate(krate)?;
                                // A crate that does not map to a `Package` cannot be in the
                                // plan, so it takes the newest registry version like any frozen
                                // crate.
                                let replacement = match Package::from_str(krate, true) {
                                    Ok(pkg) => {
                                        choose_replacement_version(&pkg, plan, &registry_versions)?
                                    }
                                    Err(_) => {
                                        registry_versions.into_iter().max().ok_or_else(|| {
                                            anyhow::anyhow!(
                                                "`{krate}` has no version in the local registry"
                                            )
                                        })?
                                    }
                                };
                                dep.1.as_table_like_mut().and_then(|table| {
                                    table.remove("path");
                                    table.insert(
                                        "version",
                                        toml_edit::Item::Value(toml_edit::Value::String(
                                            toml_edit::Formatted::new(replacement.to_string()),
                                        )),
                                    );
                                    Some(table)
                                });
                            }
                        }
                    }
                }

                let processed = format!("#{}{}\n{}", "STOP", "SHIP", toml.to_string());
                std::fs::write(manifest_path.join("Cargo.toml"), processed)?;

                // add the local registry to the config.toml
                let config = std::fs::read_to_string(manifest_path.join(".cargo/config.toml$"))?;
                if !config.contains("local-registry") {
                    std::fs::write(
                        manifest_path.join(".cargo/config.toml"),
                        format!(
                            r#"{}

# {}{}
[source.crates-io]
registry = 'sparse+https://index.crates.io/'
replace-with = 'local-registry'

[source.local-registry]
local-registry = '{}'
"#,
                            config,
                            "STOP",
                            "SHIP",
                            windows_safe_path(
                                &std::path::PathBuf::from("target/local-registry")
                                    .canonicalize()
                                    .unwrap()
                            )
                            .display()
                        ),
                    )?;
                }
            }
        }
    }

    Ok(())
}

// ---

fn topological_sort(dep_graph: &HashMap<Package, Vec<Package>>) -> Vec<Package> {
    let mut sorted = Vec::new();
    let mut dep_graph = dep_graph.clone();
    while !dep_graph.is_empty() {
        dep_graph.retain(|pkg, deps| {
            deps.retain(|dep| !sorted.contains(dep));

            if deps.is_empty() {
                sorted.push(*pkg);
                false
            } else {
                true
            }
        });
    }

    sorted
}

const EXPECTED: (u64, u64) = (0, 1);
const EXACT_ALLOWLIST: [Package; 1] = [Package::EspRadio];

/// Checks that the esp-rom-sys dependency policy is followed across the workspace.
fn check_rom_sys_policy(workspace: &Path) -> Result<()> {
    let mut errs = Vec::new();
    let mut global_exact: Option<Version> = None;
    let mut count = 0;

    for pkg in Package::iter().filter(|&p| p != Package::Examples) {
        let Ok(mut toml) = CargoToml::new(workspace, pkg) else {
            continue;
        };
        let mut found = false;

        toml.visit_dependencies(|path, _, table| {
            let Some(raw) = get_ver(table, "esp-rom-sys") else {
                return;
            };
            let Ok(req) = VersionReq::parse(&raw) else {
                return errs.push(format!("Bad semver: {raw}"));
            };

            let exact = extract_exact(&req.comparators);
            let is_allowed = EXACT_ALLOWLIST.contains(&pkg);
            found = true;
            count += 1;

            if is_allowed != exact.is_some() {
                errs.push(format!("{pkg}: pin policy mismatch for '{raw}'"));
            }

            if let Some(v) = exact {
                if global_exact.get_or_insert(v.clone()) != &v {
                    errs.push(format!("Conflicting pins: {raw}"));
                }
            }

            let in_line = req
                .comparators
                .iter()
                .all(|c| c.major == EXPECTED.0 && c.minor == Some(EXPECTED.1));
            if !in_line || req.matches(&Version::new(EXPECTED.0, EXPECTED.1 + 1, 0)) {
                errs.push(format!(
                    "{path}: must stay on {}.{}.*",
                    EXPECTED.0, EXPECTED.1
                ));
            }
        });

        if EXACT_ALLOWLIST.contains(&pkg) && !found {
            errs.push(format!("Missing required pin for {pkg:?}"));
        }
    }

    if errs.is_empty() {
        Ok(log::info!("Checked {count} deps, all good."))
    } else {
        bail!("Policy failed:\n- {}", errs.join("\n- "))
    }
}

fn get_ver(table: &Table, target: &str) -> Option<String> {
    table.iter().find_map(|(name, item)| {
        let pkg = item.get("package").and_then(|i| i.as_str()).unwrap_or(name);
        if pkg != target {
            return None;
        }
        item.as_str()
            .or_else(|| item.get("version").and_then(|v| v.as_str()))
            .map(Into::into)
    })
}

fn extract_exact(comps: &[Comparator]) -> Option<Version> {
    let c = comps.first()?;
    (comps.len() == 1 && c.op == Op::Exact).then(|| Version {
        major: c.major,
        minor: c.minor.unwrap_or(0),
        patch: c.patch.unwrap_or(0),
        pre: c.pre.clone(),
        build: Default::default(),
    })
}

#[cfg(all(test, feature = "rel-check"))]
mod tests {
    use super::*;

    fn ver(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// A finalized plan (pure JSON) with two crates. Extra fields exercise that
    /// the minimal structs ignore what they do not model.
    fn sample_plan_json() -> &'static str {
        r#"{
  "base": "main",
  "slug": "abc123",
  "packages": [
    { "package": "esp-metadata-generated", "semver_checked": false, "current_version": "0.5.1", "new_version": "0.6.0", "tag_name": "esp-metadata-generated-v0.6.0", "bump": { "base": "Minor", "pre": null } },
    { "package": "esp-hal", "semver_checked": true, "current_version": "1.2.0", "new_version": "1.3.0", "tag_name": "esp-hal-v1.3.0", "bump": { "base": "Minor", "pre": null } }
  ]
}"#
    }

    fn assert_sample(plan: &PlanFile) {
        assert_eq!(plan.packages.len(), 2);
        assert_eq!(plan.packages[0].package, Package::EspMetadataGenerated);
        assert_eq!(plan.packages[0].new_version, ver("0.6.0"));
        assert_eq!(plan.packages[1].package, Package::EspHal);
        assert_eq!(plan.packages[1].new_version, ver("1.3.0"));
    }

    #[test]
    fn parses_plain_plan_json() {
        assert_sample(&parse_plan(sample_plan_json()).unwrap());
    }

    #[test]
    fn parses_plan_with_comment_header() {
        let src = format!(
            "// generated by xtask\n//\n// edit me\n{}",
            sample_plan_json()
        );
        assert_sample(&parse_plan(&src).unwrap());
    }

    #[test]
    fn parses_plan_in_jsonc_fence() {
        let body = format!(
            "PR body preamble\n\n<details>\n\n```jsonc\n{}\n```\n\n</details>\n\ntrailing text\n\n```\nnot a plan\n```",
            sample_plan_json()
        );
        assert_sample(&parse_plan(&body).unwrap());
    }

    #[test]
    fn packages_to_package_are_the_plan_crates() {
        let plan = parse_plan(sample_plan_json()).unwrap();
        let pkgs = packages_to_package_from_tree(&plan);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains(&Package::EspMetadataGenerated));
        assert!(pkgs.contains(&Package::EspHal));
    }

    #[test]
    fn plan_crate_uses_plan_version_over_registry() {
        let plan = parse_plan(sample_plan_json()).unwrap();
        // The registry carries a newer version, but a crate in the plan must
        // resolve to its planned version regardless.
        let registry = [ver("0.6.5"), ver("0.5.1")];
        let chosen =
            choose_replacement_version(&Package::EspMetadataGenerated, &plan, &registry).unwrap();
        assert_eq!(chosen, ver("0.6.0"));
    }

    #[test]
    fn frozen_crate_uses_newest_registry_version() {
        let plan = parse_plan(sample_plan_json()).unwrap();
        let registry = [ver("0.10.0"), ver("0.11.0"), ver("0.9.0")];
        let chosen = choose_replacement_version(&Package::EspAlloc, &plan, &registry).unwrap();
        assert_eq!(chosen, ver("0.11.0"));
    }

    #[test]
    fn frozen_crate_without_registry_versions_errors() {
        let plan = parse_plan(sample_plan_json()).unwrap();
        let registry: [Version; 0] = [];
        assert!(choose_replacement_version(&Package::EspAlloc, &plan, &registry).is_err());
    }
}
