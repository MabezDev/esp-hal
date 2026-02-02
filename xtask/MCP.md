# MCP Server for esp-hal xtask

This document describes the MCP (Model Context Protocol) server implementation for the esp-hal xtask build tool, enabling AI agents to interact with the build system.

## Overview

The MCP server exposes all xtask CLI operations as tools that can be called by AI agents like GitHub Copilot, Claude, or other MCP-compatible clients. Communication happens over stdio using JSON-RPC 2.0.

## Building with MCP Support

The MCP server is behind the `mcp` feature flag. To build xtask with MCP support:

```bash
cd esp-hal
cargo build -p xtask --features mcp
```

## Starting the MCP Server

Using the cargo alias (recommended):

```bash
cargo xmcp
```

Or explicitly with features:

```bash
cargo xtask --features mcp mcp
```

Or if you have a pre-built binary with MCP support:

```bash
./target/debug/xtask mcp
```

## Configuration for AI Clients

### VS Code with GitHub Copilot

The workspace already includes `.vscode/mcp.json` which is auto-discovered by VS Code. If you need to create it manually:

```json
{
  "servers": {
    "esp-hal-xtask": {
      "type": "stdio",
      "command": "cargo",
      "args": ["xmcp"],
      "cwd": "${workspaceFolder}"
    }
  }
}
```

### Claude Desktop

Add to your Claude Desktop config (`~/.config/claude/config.json` on Linux, `~/Library/Application Support/Claude/config.json` on macOS):

```json
{
  "mcpServers": {
    "esp-hal-xtask": {
      "command": "cargo",
      "args": ["xtask", "--features", "mcp", "mcp"],
      "cwd": "/path/to/esp-hal"
    }
  }
}
```

## Available Tools

The MCP server exposes the following tools:

### Build Tools

| Tool | Description |
|------|-------------|
| `build-documentation` | Build documentation for esp-hal packages |
| `build-examples` | Build examples for a specific chip |
| `build-package` | Build a specific package with custom options |
| `build-tests` | Build tests for a specific chip |

### Run Tools

| Tool | Description |
|------|-------------|
| `run-doc-tests` | Run documentation tests for a chip |
| `run-example` | Run an example for a specific chip |
| `run-tests` | Run tests for a specific chip |

### Code Quality Tools

| Tool | Description |
|------|-------------|
| `fmt-packages` | Format all packages in the workspace with rustfmt |
| `lint-packages` | Lint all packages in the workspace with clippy |
| `check-packages` | Check all packages with cargo check |
| `check-changelog` | Check the changelog for packages |
| `host-tests` | Run host tests in the workspace |

### CI Tools

| Tool | Description |
|------|-------------|
| `ci` | Run CI checks for a specific chip |
| `clean` | Clean build artifacts for packages |

### Metadata Tools

| Tool | Description |
|------|-------------|
| `update-metadata` | Re-generate metadata and the chip support table |

### Semver Tools

| Tool | Description |
|------|-------------|
| `semver-check` | Run semver checks on packages |

### Release Tools

| Tool | Description |
|------|-------------|
| `bump-version` | Bump the version of specified packages |
| `publish` | Publish a package to crates.io |
| `tag-releases` | Generate git tags for package releases |

### Utility Tools

| Tool | Description |
|------|-------------|
| `list-packages` | List all available packages in the workspace |
| `list-chips` | List all supported ESP32 chips |
| `help` | Get help text for xtask CLI commands |

## Supported Chips

The following chips are supported:

- `esp32` - Xtensa LX6 dual-core, WiFi, Bluetooth Classic, BLE
- `esp32c2` - RISC-V single-core, WiFi, BLE
- `esp32c3` - RISC-V single-core, WiFi, BLE
- `esp32c6` - RISC-V single-core, WiFi 6, BLE 5, 802.15.4
- `esp32h2` - RISC-V single-core, BLE 5, 802.15.4
- `esp32s2` - Xtensa LX7 single-core, WiFi
- `esp32s3` - Xtensa LX7 dual-core, WiFi, BLE

## Example Usage from an AI Agent

### Format and lint before PR

```
1. Call `fmt-packages` to format all code
2. Call `lint-packages` to check for issues
3. Call `check-changelog` to verify changelog entries
```

### Build examples for a chip

```
Call `build-examples` with:
- chip: "esp32c6"
- example: "blinky" (or omit for all examples)
```

### Run CI checks locally

```
Call `ci` with:
- chip: "esp32c6"
```

## Adding New Tools

When new commands are added to xtask, the MCP server should be updated:

1. Add a new method in `xtask/src/mcp/tools.rs` with the `#[tool]` attribute
2. The method should call `self.run_xtask_command()` with the appropriate arguments
3. Document the tool's parameters using `#[tool(param)]` attributes

The tool naming convention is kebab-case (e.g., `build-examples`, `fmt-packages`).

## Protocol Details

- **Transport**: stdio (stdin/stdout)
- **Protocol**: JSON-RPC 2.0
- **MCP Version**: Latest (as per rmcp crate)

## Troubleshooting

### Server doesn't start

Ensure you have the `mcp` feature enabled:
```bash
cargo build -p xtask --features mcp
```

### Commands fail

The MCP server runs xtask commands in the workspace directory. Ensure:
1. You're in the esp-hal workspace root
2. Required toolchains are installed (esp toolchain for Xtensa chips)
3. Environment is properly set up (see main xtask README)

### Timeouts

Some operations (like building all examples) can take a long time. The MCP server doesn't impose timeouts, but clients might. Consider:
1. Building for specific chips instead of all
2. Building specific examples instead of all
3. Using `--debug` flag for faster builds
