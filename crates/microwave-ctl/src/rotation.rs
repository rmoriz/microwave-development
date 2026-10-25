//! Service credential rotation commands.

use crate::{context::Context, CtlError};

pub fn rotate_credentials(_context: &Context, _service: &str) -> Result<(), CtlError> {
    Err(CtlError::message(
        "rotate-credentials is not implemented yet",
    ))
}

pub fn rotate_all_credentials(_context: &Context) -> Result<(), CtlError> {
    Err(CtlError::message(
        "rotate-all-credentials is not implemented yet",
    ))
}

pub fn rotate_shared_secrets(_context: &Context) -> Result<(), CtlError> {
    Err(CtlError::message(
        "rotate-shared-secrets is not implemented yet",
    ))
}
