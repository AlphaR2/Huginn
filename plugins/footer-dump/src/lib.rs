//! Geyser plugin that records Alpenglow block footers.
//!
//! The validator calls `notify_block_footer` once per block it replays or produces. We write
//! one JSON line per footer with the raw certificate bytes (hex), so `alpenglow-lc` can verify
//! them offline. Nothing is decoded or checked here: the plugin only copies bytes out.
//!
//! Config file (passed with `--geyser-plugin-config`):
//!   { "libpath": "target/release/libfooter_dump.dylib", "out": "fixtures/local/footers.jsonl" }

use {
    agave_geyser_plugin_interface::{
        block_footer::{BlockFinalizationCert, VersionedBlockFooter, VotesAggregate},
        geyser_plugin_interface::{
            GeyserPlugin, GeyserPluginError, ReplicaBlockFooterInfoVersions, Result,
        },
    },
    serde_json::{json, Value},
    std::{
        fs::{File, OpenOptions},
        io::Write,
        sync::Mutex,
    },
};

#[derive(Debug, Default)]
pub struct FooterDump {
    out: Option<Mutex<File>>,
}

fn aggregate(a: &VotesAggregate) -> Value {
    json!({
        "signature": hex::encode(a.signature.0),
        "bitmap": hex::encode(a.bitmap),
    })
}

fn final_cert(c: &BlockFinalizationCert) -> Value {
    json!({
        "slot": c.slot,
        "block_id": hex::encode(c.block_id.as_ref()),
        "final_aggregate": aggregate(&c.final_aggregate),
        "notar_aggregate": c.notar_aggregate.as_ref().map(aggregate),
    })
}

impl GeyserPlugin for FooterDump {
    fn name(&self) -> &'static str {
        "footer-dump"
    }

    fn on_load(&mut self, config_file: &str, _is_reload: bool) -> Result<()> {
        let config: Value = serde_json::from_str(&std::fs::read_to_string(config_file)?)
            .map_err(|e| GeyserPluginError::ConfigFileReadError { msg: e.to_string() })?;
        let out = config["out"].as_str().ok_or_else(|| GeyserPluginError::ConfigFileReadError {
            msg: "missing \"out\" path".into(),
        })?;
        let file = OpenOptions::new().create(true).append(true).open(out)?;
        self.out = Some(Mutex::new(file));
        Ok(())
    }

    fn notify_block_footer(
        &self,
        block_footer: ReplicaBlockFooterInfoVersions,
        bank_id: u64,
    ) -> Result<()> {
        let ReplicaBlockFooterInfoVersions::V0_0_2(info) = block_footer;
        let VersionedBlockFooter::V1(footer) = info.block_footer;
        let line = json!({
            "slot": info.slot,
            "bank_id": bank_id,
            "bank_hash": hex::encode(footer.bank_hash.as_ref()),
            "block_final_cert": footer.block_final_cert.as_ref().map(final_cert),
        });
        let Some(out) = &self.out else { return Ok(()) };
        // A poisoned lock only means another write panicked; keep writing.
        let mut file = out.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(file, "{line}")?;
        Ok(())
    }

    fn block_footer_notifications_enabled(&self) -> bool {
        true
    }

    // Opt out of everything else, so the validator doesn't do extra work for us.
    fn account_data_notifications_enabled(&self) -> bool {
        false
    }

    fn transaction_notifications_enabled(&self) -> bool {
        false
    }
}

/// Entry point the validator looks up by name when it loads the library.
///
/// # Safety
/// The validator takes ownership of the returned pointer.
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub unsafe extern "C" fn _create_plugin() -> *mut dyn GeyserPlugin {
    Box::into_raw(Box::new(FooterDump::default()))
}
