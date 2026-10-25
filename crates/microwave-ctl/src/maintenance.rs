//! Authorization cleanup commands.

use crate::{context::Context, CtlError};

pub fn cleanup_bifrost(
    _context: &Context,
    _estimate_only: bool,
    _wait_time: u64,
    _force_cleanup: bool,
) -> Result<(), CtlError> {
    Err(CtlError::message("cleanup-bifrost is not implemented yet"))
}
