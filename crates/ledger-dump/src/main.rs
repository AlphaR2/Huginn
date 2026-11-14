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
    clap::{Parser, Subcommand},
    ledger_dump::{
        build_fixture, check_set, export, newest_certified, Certs, Ledger, Sets, ShredFiles,
        ShredSource,
    },
    std::{fs, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Write a checked block fixture (set, certs, shreds) for a recent slot")]
struct Cli {
    /// Ledger directory (the one passed to the validator with --ledger).
    #[arg(long, global = true, default_value = ".local-ledger")]
    ledger: PathBuf,
    /// Read shreds from this directory of `shreds_<slot>.json` files instead of a ledger.
    #[arg(long, global = true)]
    shreds: Option<PathBuf>,
    /// Slot to dump. Defaults to the most recent slot with a certificate.
    #[arg(long, global = true)]
    slot: Option<u64>,
    /// Only print the slot range the source holds.
    #[arg(long, global = true)]
    range: bool,
    /// Instead of a fixture, write `shreds_<slot>.json` for slots FROM..=TO (e.g. 100..102).
    #[arg(long, global = true)]
    export_shreds: Option<String>,
    /// Where certificates come from: `shreds` (the next block's footer) or `plugin`
    /// (footer-dump's output, `--footers`).
    #[arg(long, global = true, default_value = "shreds")]
    certs: String,
    /// footer-dump plugin output, for `--certs plugin`.
    #[arg(long, global = true, default_value = "fixtures/local/footers.jsonl")]
    footers: PathBuf,
    /// Where `cluster_info.json` and `validator_set_<epoch>.json` live (from `fetch`).
    #[arg(long, global = true, default_value = "fixtures/local")]
    sets: PathBuf,
    /// Epoch length. `solana-test-validator` uses 432,000 with no warmup.
    #[arg(long, global = true, default_value_t = 432_000)]
    slots_per_epoch: u64,
    /// Output directory.
    #[arg(long, global = true, default_value = "fixtures/local")]
    out: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Verify the validator set in `--sets` against the footer certificates of `--check-slots`
    /// (default: every block the source holds). Writes a pass/fail JSON report and exits
    /// non-zero on a fail. It never changes the set.
    CheckSet {
        /// Blocks whose footer certificates must verify (comma separated).
        #[arg(long, value_delimiter = ',')]
        check_slots: Vec<u64>,
        /// At least this many distinct certificates must verify.
        #[arg(long, default_value_t = 3)]
        min_certs: usize,
        /// Report path. Default: `<sets>/check_set_<epoch>.json`.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", redact::redact_text(&format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
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

    let sets = Sets {
        dir: cli.sets.clone(),
        slots_per_epoch: cli.slots_per_epoch,
    };
    if let Some(Cmd::CheckSet { check_slots, min_certs, report }) = &cli.cmd {
        let slots: Vec<u64> = if check_slots.is_empty() {
            let (a, b) = src.range()?;
            (a..=b).collect()
        } else {
            check_slots.clone()
        };
        let rep = check_set(src.as_ref(), &slots, &sets, *min_certs)?;
        let path = report.clone().unwrap_or_else(|| {
            sets.dir.join(format!("check_set_{}.json", rep["epoch"]))
        });
        fs::write(&path, serde_json::to_string_pretty(&rep)? + "\n")?;
        println!(
            "{} set for epoch {} ({} validators): {} certificate(s) verified, {} failed -> {}",
            if rep["pass"] == true { "PASS" } else { "FAIL" },
            rep["epoch"],
            rep["validators"],
            rep["certificates"]["verified"],
            rep["certificates"]["failed"],
            path.display()
        );
        if rep["pass"] != true {
            bail!("the validator set does not verify the certificates (the set file was not changed)");
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
