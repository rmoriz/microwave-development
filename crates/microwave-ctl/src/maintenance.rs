//! Authorization cleanup commands.

use crate::{context::Context, CtlError};
use std::time::Duration;

/// Remove authorization actors and objects that no identity or object table
/// references.
///
/// `estimate_only` reports the number of orphaned `authz_entities` rows
/// without changing anything. `wait_time` gives in-flight requests time to
/// finish before the count/delete (zero skips the wait). `force_cleanup` is
/// accepted for `chef-server-ctl` compatibility; Microwave has no legacy
/// cleanup tracking tables, so it only reports that fact.
///
/// The command is deliberately non-interactive: `cleanup-bifrost` has no
/// `--yes` flag upstream, and every invocation prints the resulting count.
pub fn cleanup_bifrost(
    context: &Context,
    estimate_only: bool,
    wait_time: u64,
    force_cleanup: bool,
) -> Result<(), CtlError> {
    if wait_time > 0 {
        println!(
            "Waiting {wait_time} second(s) for in-flight requests before cleaning authorization data..."
        );
        std::thread::sleep(Duration::from_secs(wait_time));
    }

    let store = context.open_metadata_store()?;
    if estimate_only {
        let count = store.run(store.store().count_orphan_authz_entities())?;
        println!("Unused authorization entities: {count} (estimate only; nothing deleted)");
    } else {
        let removed = store.run(store.store().delete_orphan_authz_entities())?;
        println!("Removed {removed} unused authorization entities.");
    }

    if force_cleanup {
        println!(
            "No legacy authorization cleanup tracking tables exist in Microwave; \
             --force-cleanup has nothing extra to remove."
        );
    }

    Ok(())
}
