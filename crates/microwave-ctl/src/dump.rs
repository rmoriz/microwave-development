//! Filtered metadata dumps and log collection.

use crate::{context::Context, CtlError};
use std::path::Path;

pub fn filtered_dump(
    _context: &Context,
    _output: &Path,
    _org: Option<&str>,
) -> Result<(), CtlError> {
    Err(CtlError::message("filtered-dump is not implemented yet"))
}

pub fn gather_logs(_context: &Context, _output: Option<&Path>) -> Result<(), CtlError> {
    Err(CtlError::message("gather-logs is not implemented yet"))
}
