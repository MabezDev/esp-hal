use std::path::Path;

use clap::{Args, Subcommand};
use anyhow::Result;
use crate::{publish, Stage};

// ----------------------------------------------------------------------------
// Subcommands

#[derive(Debug, Subcommand)]
pub enum Release {
    /// Publish all crates in a given stage
    Publish(ReleasePublishArgs),
}


// ----------------------------------------------------------------------------
// Subcommand Arguments

#[derive(Debug, Args)]
pub struct ReleasePublishArgs {
    #[arg(value_enum)]
    pub stage: Stage,
    /// Do not pass the `--dry-run` argument, actually try to publish.
    #[arg(long)]
    no_dry_run: bool,
}

// ---------------------------------------------------------------------------
// Subcommand Actions

pub fn release(workspace: &Path, args: ReleasePublishArgs) -> Result<()> {
    for package in args.stage.packages() {
        publish(workspace, package, args.no_dry_run)?;
    }
    Ok(())
}