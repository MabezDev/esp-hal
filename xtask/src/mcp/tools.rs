//! MCP tool definitions for xtask commands.
//!
//! This module defines MCP tools that wrap xtask CLI commands, making them
//! accessible to AI agents via the Model Context Protocol.

use std::path::PathBuf;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};

/// MCP server that exposes xtask operations as tools.
#[derive(Clone)]
pub struct XtaskMcpServer {
    workspace: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl XtaskMcpServer {
    /// Execute a cargo xtask command and capture output.
    fn run_xtask_command(&self, args: &[&str]) -> String {
        use std::process::Command;

        let output = match Command::new("cargo")
            .arg("xtask")
            .args(args)
            .current_dir(&self.workspace)
            .output()
        {
            Ok(output) => output,
            Err(e) => return format!("Failed to execute command: {}", e),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            format!("{}\n{}", stdout, stderr)
        } else {
            format!(
                "Command failed with status {}:\nstdout: {}\nstderr: {}",
                output.status, stdout, stderr
            )
        }
    }
}

// ============================================================================
// Parameter structures for tools
// ============================================================================

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildDocumentationParams {
    /// Comma-separated list of packages to document (e.g., "esp-hal,esp-radio").
    pub packages: Option<String>,
    /// Comma-separated list of chips to build docs for (e.g., "esp32,esp32c3").
    pub chips: Option<String>,
    /// Base URL for deployed documentation links.
    pub base_url: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildExamplesParams {
    /// Name of the example to build, or omit to build all examples.
    pub example: Option<String>,
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: Option<String>,
    /// Package containing the examples (defaults to "examples").
    pub package: Option<String>,
    /// Build in debug mode only.
    pub debug: Option<bool>,
    /// Toolchain to use for building.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildPackageParams {
    /// Package to build (e.g., "esp-hal", "esp-alloc").
    pub package: String,
    /// Target triple to build for.
    pub target: Option<String>,
    /// Comma-separated list of features to enable.
    pub features: Option<String>,
    /// Toolchain to use for building.
    pub toolchain: Option<String>,
    /// Disable default features.
    pub no_default_features: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildTestsParams {
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: String,
    /// Specific test(s) to build (comma-separated).
    pub test: Option<String>,
    /// Toolchain to use for building.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RunDocTestsParams {
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: String,
    /// Comma-separated list of packages to test.
    pub packages: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RunExampleParams {
    /// Name of the example to run.
    pub example: String,
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: Option<String>,
    /// Package containing the example.
    pub package: Option<String>,
    /// Toolchain to use.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RunTestsParams {
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: String,
    /// Specific test(s) to run (comma-separated).
    pub test: Option<String>,
    /// Number of times to repeat the tests.
    pub repeat: Option<u32>,
    /// Toolchain to use.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FmtPackagesParams {
    /// Run in check mode (exit with error if not formatted).
    pub check: Option<bool>,
    /// Comma-separated list of packages to format.
    pub packages: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LintPackagesParams {
    /// Comma-separated list of packages to lint.
    pub packages: Option<String>,
    /// Comma-separated list of chips to lint for.
    pub chips: Option<String>,
    /// Automatically apply fixes.
    pub fix: Option<bool>,
    /// Toolchain to use.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CheckPackagesParams {
    /// Comma-separated list of packages to check.
    pub packages: Option<String>,
    /// Comma-separated list of chips to check for.
    pub chips: Option<String>,
    /// Toolchain to use.
    pub toolchain: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CheckChangelogParams {
    /// Comma-separated list of packages to check.
    pub packages: Option<String>,
    /// Re-generate changelogs with consistent formatting.
    pub normalize: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HostTestsParams {
    /// Comma-separated list of packages to test.
    pub packages: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateMetadataParams {
    /// Run in check mode (exit with error if changes needed).
    pub check: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CiParams {
    /// Target chip (e.g., "esp32", "esp32c3", "esp32c6").
    pub chip: String,
    /// Toolchain to use.
    pub toolchain: Option<String>,
    /// Skip running lints.
    pub no_lint: Option<bool>,
    /// Skip building documentation.
    pub no_docs: Option<bool>,
    /// Skip checking crates.
    pub no_check_crates: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CleanParams {
    /// Comma-separated list of packages to clean.
    pub packages: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SemverCheckParams {
    /// Subcommand: "generate-baseline", "check", or "download-baselines".
    pub action: String,
    /// Comma-separated list of packages.
    pub packages: Option<String>,
    /// Comma-separated list of chips.
    pub chips: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BumpVersionParams {
    /// Version bump type: "major", "minor", or "patch".
    pub bump: String,
    /// Comma-separated list of packages to bump.
    pub packages: Option<String>,
    /// Dry run (show what would change without making changes).
    pub dry_run: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublishParams {
    /// Package to publish.
    pub package: String,
    /// Dry run (validate without publishing).
    pub dry_run: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TagReleasesParams {
    /// Comma-separated list of packages to tag.
    pub packages: Option<String>,
    /// Dry run (show what would be tagged).
    pub dry_run: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HelpParams {
    /// Subcommand to get help for (e.g., "build", "run", "release").
    pub command: Option<String>,
}

// ============================================================================
// Tool implementations
// ============================================================================

#[tool_router]
impl XtaskMcpServer {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Build documentation for esp-hal packages")]
    fn build_documentation(
        &self,
        Parameters(params): Parameters<BuildDocumentationParams>,
    ) -> String {
        let mut args = vec!["build", "documentation"];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        if let Some(ref c) = params.chips {
            args.push("--chips");
            args.push(c);
        }
        if let Some(ref u) = params.base_url {
            args.push("--base-url");
            args.push(u);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Build examples for a specific chip")]
    fn build_examples(&self, Parameters(params): Parameters<BuildExamplesParams>) -> String {
        let mut args = vec!["build", "examples"];
        if let Some(ref e) = params.example {
            args.push(e);
        }
        if let Some(ref c) = params.chip {
            args.push("--chip");
            args.push(c);
        }
        if let Some(ref p) = params.package {
            args.push("--package");
            args.push(p);
        }
        if params.debug == Some(true) {
            args.push("--debug");
        }
        if let Some(ref t) = params.toolchain {
            args.push("--toolchain");
            args.push(t);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Build a specific package with custom options")]
    fn build_package(&self, Parameters(params): Parameters<BuildPackageParams>) -> String {
        let mut args = vec!["build", "package", &params.package];
        if let Some(ref t) = params.target {
            args.push("--target");
            args.push(t);
        }
        if let Some(ref f) = params.features {
            args.push("--features");
            args.push(f);
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        if params.no_default_features == Some(true) {
            args.push("--no-default-features");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Build tests for a specific chip")]
    fn build_tests(&self, Parameters(params): Parameters<BuildTestsParams>) -> String {
        let mut args = vec!["build", "tests", &params.chip];
        if let Some(ref t) = params.test {
            args.push("--test");
            args.push(t);
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run documentation tests for a chip")]
    fn run_doc_tests(&self, Parameters(params): Parameters<RunDocTestsParams>) -> String {
        let mut args = vec!["run", "doc-tests", &params.chip];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run an example for a specific chip")]
    fn run_example(&self, Parameters(params): Parameters<RunExampleParams>) -> String {
        let mut args = vec!["run", "example", &params.example];
        if let Some(ref c) = params.chip {
            args.push("--chip");
            args.push(c);
        }
        if let Some(ref p) = params.package {
            args.push("--package");
            args.push(p);
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run tests for a specific chip")]
    fn run_tests(&self, Parameters(params): Parameters<RunTestsParams>) -> String {
        let mut args = vec!["run", "tests", &params.chip];
        if let Some(ref t) = params.test {
            args.push("--test");
            args.push(t);
        }
        let repeat_str;
        if let Some(r) = params.repeat {
            repeat_str = r.to_string();
            args.push("--repeat");
            args.push(&repeat_str);
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Format all packages in the workspace with rustfmt")]
    fn fmt_packages(&self, Parameters(params): Parameters<FmtPackagesParams>) -> String {
        let mut args = vec!["fmt-packages"];
        if params.check == Some(true) {
            args.push("--check");
        }
        if let Some(ref p) = params.packages {
            args.push(p);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Lint all packages in the workspace with clippy")]
    fn lint_packages(&self, Parameters(params): Parameters<LintPackagesParams>) -> String {
        let mut args = vec!["lint-packages"];
        if let Some(ref p) = params.packages {
            args.push(p);
        }
        if let Some(ref c) = params.chips {
            args.push("--chips");
            args.push(c);
        }
        if params.fix == Some(true) {
            args.push("--fix");
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Check all packages with cargo check")]
    fn check_packages(&self, Parameters(params): Parameters<CheckPackagesParams>) -> String {
        let mut args = vec!["check-packages"];
        if let Some(ref p) = params.packages {
            args.push(p);
        }
        if let Some(ref c) = params.chips {
            args.push("--chips");
            args.push(c);
        }
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Check the changelog for packages")]
    fn check_changelog(
        &self,
        Parameters(params): Parameters<CheckChangelogParams>,
    ) -> String {
        let mut args = vec!["check-changelog"];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        if params.normalize == Some(true) {
            args.push("--normalize");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run host tests in the workspace")]
    fn host_tests(&self, Parameters(params): Parameters<HostTestsParams>) -> String {
        let mut args = vec!["host-tests"];
        if let Some(ref p) = params.packages {
            args.push(p);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Re-generate metadata and the chip support table")]
    fn update_metadata(&self, Parameters(params): Parameters<UpdateMetadataParams>) -> String {
        let mut args = vec!["update-metadata"];
        if params.check == Some(true) {
            args.push("--check");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run CI checks for a specific chip")]
    fn ci(&self, Parameters(params): Parameters<CiParams>) -> String {
        let mut args = vec!["ci", &params.chip];
        if let Some(ref tc) = params.toolchain {
            args.push("--toolchain");
            args.push(tc);
        }
        if params.no_lint == Some(true) {
            args.push("--no-lint");
        }
        if params.no_docs == Some(true) {
            args.push("--no-docs");
        }
        if params.no_check_crates == Some(true) {
            args.push("--no-check-crates");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Clean build artifacts for packages")]
    fn clean(&self, Parameters(params): Parameters<CleanParams>) -> String {
        let mut args = vec!["clean"];
        if let Some(ref p) = params.packages {
            args.push(p);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Run semver checks on packages")]
    fn semver_check(&self, Parameters(params): Parameters<SemverCheckParams>) -> String {
        let mut args = vec!["semver-check", &params.action];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        if let Some(ref c) = params.chips {
            args.push("--chips");
            args.push(c);
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Bump the version of specified packages")]
    fn bump_version(&self, Parameters(params): Parameters<BumpVersionParams>) -> String {
        let mut args = vec!["release", "bump-version", &params.bump];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        if params.dry_run == Some(true) {
            args.push("--dry-run");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Publish a package to crates.io")]
    fn publish(&self, Parameters(params): Parameters<PublishParams>) -> String {
        let mut args = vec!["release", "publish", &params.package];
        if params.dry_run == Some(true) {
            args.push("--dry-run");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "Generate git tags for package releases")]
    fn tag_releases(&self, Parameters(params): Parameters<TagReleasesParams>) -> String {
        let mut args = vec!["release", "tag-releases"];
        if let Some(ref p) = params.packages {
            args.push("--packages");
            args.push(p);
        }
        if params.dry_run == Some(true) {
            args.push("--dry-run");
        }
        self.run_xtask_command(&args)
    }

    #[tool(description = "List all available packages in the workspace")]
    fn list_packages(&self) -> String {
        let packages = [
            "esp-alloc",
            "esp-backtrace",
            "esp-bootloader-esp-idf",
            "esp-config",
            "esp-hal",
            "esp-hal-procmacros",
            "esp-rom-sys",
            "esp-lp-hal",
            "esp-metadata",
            "esp-metadata-generated",
            "esp-phy",
            "esp-println",
            "esp-riscv-rt",
            "esp-storage",
            "esp-sync",
            "esp-radio",
            "esp-radio-rtos-driver",
            "esp-rtos",
            "examples",
            "hil-test",
            "qa-test",
            "xtensa-lx",
            "xtensa-lx-rt",
            "xtensa-lx-rt-proc-macros",
        ];
        packages.join("\n")
    }

    #[tool(description = "List all supported ESP32 chips")]
    fn list_chips(&self) -> String {
        let chips = [
            "esp32 - Xtensa LX6 dual-core, WiFi, Bluetooth Classic, BLE",
            "esp32c2 - RISC-V single-core, WiFi, BLE",
            "esp32c3 - RISC-V single-core, WiFi, BLE",
            "esp32c6 - RISC-V single-core, WiFi 6, BLE 5, 802.15.4",
            "esp32h2 - RISC-V single-core, BLE 5, 802.15.4",
            "esp32s2 - Xtensa LX7 single-core, WiFi",
            "esp32s3 - Xtensa LX7 dual-core, WiFi, BLE",
        ];
        chips.join("\n")
    }

    #[tool(description = "Get help text for xtask CLI commands")]
    fn help(&self, Parameters(params): Parameters<HelpParams>) -> String {
        let mut args = vec![];
        if let Some(ref cmd) = params.command {
            args.push(cmd.as_str());
        }
        args.push("--help");
        self.run_xtask_command(&args)
    }
}

#[tool_handler]
impl ServerHandler for XtaskMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                r#"This MCP server provides access to the esp-hal xtask build system.

## Quick Start

1. **Format code** before submitting: `fmt_packages`
2. **Lint code**: `lint_packages`
3. **Check changelog**: `check_changelog`
4. **Build examples**: `build_examples` with chip parameter

## Common Workflows

### Before Submitting a PR
1. Run `fmt_packages` to format all code
2. Run `lint_packages` to check for issues
3. Run `check_changelog` to verify changelog entries
4. Run `build_examples` for affected chips

### Building for a Chip
Use `build_examples` with the `chip` parameter set to one of:
esp32, esp32c2, esp32c3, esp32c6, esp32h2, esp32s2, esp32s3

### Running CI Locally
Use `ci` with a chip to run the same checks as CI.

## Available Chips
- esp32, esp32c2, esp32c3, esp32c6, esp32h2, esp32s2, esp32s3

## Key Packages
- esp-hal: Main HAL crate
- esp-radio: WiFi, BLE, IEEE 802.15.4 support
- esp-alloc, esp-backtrace, esp-println: Support crates
"#
                .into(),
            ),
            ..Default::default()
        }
    }
}
