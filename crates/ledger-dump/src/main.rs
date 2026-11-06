//! Writes a self-contained, checked block fixture for a recent slot, from a shred source:
//! an Agave ledger (default, read-only, safe while the validator runs) or a directory of
//! `shreds_<slot>.json` files (`--shreds`). See `lib.rs`.
//!
//! Certificates come from the shreds themselves by default (`--certs shreds`): block S's
//! footer carries the certificate that finalized its parent, so S's own certificate is in
//! its child's footer. `--certs plugin` reads them from the footer-dump plugin instead.
//! Nothing is written unless the certificates verify and the shreds rebuild the
//! certified block_id.

use {
    anyhow::{bail, Context, Result},
    clap::Parser,
    ledger_dump::{
        build_fixture, export, newest_certified, Certs, Ledger, Sets, ShredFiles, ShredSource,
    },
    std::{fs, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Write a checked block fixture (set, certs, shreds) for a recent slot")]
struct Cli {
    /// Ledger directory (the one passed to the validator with --ledger).
    #[arg(long, default_value = ".local-ledger")]
    ledger: PathBuf,
    /// Read shreds from this directory of `shreds_<slot>.json` files instead of a ledger.
    #[arg(long)]
    shreds: Option<PathBuf>,
    /// Slot to dump. Defaults to the most recent slot with a certificate.
    #[arg(long)]
    slot: Option<u64>,
    /// Only print the slot range the source holds.
    #[arg(long)]
    range: bool,
    /// Instead of a fixture, write `shreds_<slot>.json` for slots FROM..=TO (e.g. 100..102).
    #[arg(long)]
    export_shreds: Option<String>,
    /// Where certificates come from: `shreds` (the next block's footer) or `plugin`
    /// (footer-dump's output, `--footers`).
    #[arg(long, default_value = "shreds")]
    certs: String,
    /// footer-dump plugin output, for `--certs plugin`.
    #[arg(long, default_value = "fixtures/local/footers.jsonl")]
    footers: PathBuf,
    /// Where `cluster_info.json` and `validator_set_<epoch>.json` live (from `fetch`).
    #[arg(long, default_value = "fixtures/local")]
    sets: PathBuf,
    /// Epoch length. `solana-test-validator` uses 432,000 with no warmup.
    #[arg(long, default_value_t = 432_000)]
    slots_per_epoch: u64,
    /// Output directory.
    #[arg(long, default_value = "fixtures/local")]
    out: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let src: Box<dyn ShredSource> = match &cli.shreds {
        Some(dir) => Box::new(ShredFiles::new(dir)),
        None => Box::new(Ledger::open(&cli.ledger)?),
    };

    if cli.range {
        let (first, last) = src.range()?;
        println!("data shreds for slots {first} ..= {last}");
        return Ok(());
    }
    if let Some(r) = &cli.export_shreds {
        let (a, b) = r.split_once("..").context("--export-shreds FROM..TO")?;
        for slot in a.parse::<u64>()?..=b.trim_start_matches('=').parse::<u64>()? {
            println!("{}", export(src.as_ref(), slot, &cli.out)?.display());
        }
        return Ok(());
    }

    let certs = match cli.certs.as_str() {
        "shreds" => Certs::Shreds,
        "plugin" => Certs::plugin(&cli.footers)?,
        other => bail!("--certs is shreds or plugin, not {other}"),
    };
    let slot = match cli.slot {
        Some(s) => s,
        None => newest_certified(src.as_ref(), &certs)?,
    };
    let sets = Sets {
        dir: cli.sets.clone(),
        slots_per_epoch: cli.slots_per_epoch,
    };
    let built = build_fixture(src.as_ref(), slot, &sets, &certs)?;
    for line in &built.report {
        println!("{line}");
    }
    fs::create_dir_all(&cli.out)?;
    let out = cli.out.join(format!("block_{slot}.json"));
    fs::write(&out, serde_json::to_string_pretty(&built.fixture)? + "\n")?;
    println!("MATCH -> {}", out.display());
    Ok(())
}
