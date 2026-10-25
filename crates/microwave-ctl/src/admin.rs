//! User and OAuth application administration commands.

use crate::{context::Context, CtlError};
use std::path::Path;

pub fn user_edit(_context: &Context, _user: &str, _input: Option<&Path>) -> Result<(), CtlError> {
    Err(CtlError::message("user-edit is not implemented yet"))
}

pub fn oc_id_show_app(_context: &Context, _app: &str) -> Result<(), CtlError> {
    Err(CtlError::message("oc-id-show-app is not implemented yet"))
}
