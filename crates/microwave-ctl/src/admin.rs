//! User and OAuth application administration commands.

use crate::{commands::find_user, context::Context, CtlError};
use microwave_crypto::hash_password;
use microwave_store::{StoreError, User, UserPatch};
use serde_json::Value;
use std::io::Write;
use std::path::Path;

/// Fields accepted in the editable user document. Anything else is rejected so
/// a typo cannot silently leave the intended change unapplied.
const EDITABLE_FIELDS: &[&str] = &[
    "username",
    "email",
    "password",
    "admin",
    "recovery_authentication_enabled",
    "external_authentication_uid",
    "display_name",
    "first_name",
    "middle_name",
    "last_name",
];

/// Parsed and type-checked contents of a user document.
///
/// The outer `Option` means the field was present; for optional strings the
/// inner `Option` is the value (`None` clears it). `username` cannot be
/// cleared, so a present value is always a string.
#[derive(Debug, Default)]
struct UserEditDocument {
    username: Option<String>,
    email: Option<Option<String>>,
    password: Option<String>,
    admin: Option<bool>,
    recovery_authentication_enabled: Option<bool>,
    external_authentication_uid: Option<Option<String>>,
    display_name: Option<Option<String>>,
    first_name: Option<Option<String>>,
    middle_name: Option<Option<String>>,
    last_name: Option<Option<String>>,
}

pub fn user_edit(context: &Context, username: &str, input: Option<&Path>) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;

    let document = match input {
        Some(path) => read_document_file(path)?,
        None => {
            let edited = edit_document(&user)?;
            parse_document(&edited, "the edited user document")?
        }
    };

    if username.eq_ignore_ascii_case(context.superuser_name()) && document.admin == Some(false) {
        return Err(CtlError::message(format!(
            "You cannot remove the base superuser {} from server-admins.",
            context.superuser_name()
        )));
    }

    let (patch, changed_fields) = build_patch(&user, &document)?;
    let updated = store.run(store.store().update_user_patch(username, &patch))?;

    println!("Updated user {}", updated.username);
    if changed_fields.is_empty() {
        println!("changed_fields: (none)");
    } else {
        println!("changed_fields: {}", changed_fields.join(", "));
    }
    Ok(())
}

pub fn oc_id_show_app(context: &Context, app: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let application = store
        .run(store.store().list_oauth_applications())?
        .into_iter()
        .find(|application| application.name == app)
        .ok_or_else(|| StoreError::NotFound(format!("OAuth application {app}")))?;

    // Keep the upstream field order and reveal the client secret: this command
    // exists precisely to retrieve it.
    let fields = [
        ("name", Value::String(application.name)),
        ("uid", Value::String(application.uid)),
        ("secret", Value::String(application.secret)),
        ("redirect_uri", Value::String(application.redirect_uri)),
        ("scopes", Value::String(application.scopes)),
        ("confidential", Value::Bool(application.confidential)),
    ];
    let mut output = String::from("{\n");
    for (index, (name, value)) in fields.iter().enumerate() {
        if index > 0 {
            output.push_str(",\n");
        }
        output.push_str("  ");
        output.push_str(&serde_json::to_string(name).map_err(json_error)?);
        output.push_str(": ");
        output.push_str(&to_json(value)?);
    }
    output.push_str("\n}");
    println!("{output}");
    Ok(())
}

fn to_json(value: &Value) -> Result<String, CtlError> {
    serde_json::to_string(value).map_err(json_error)
}

fn json_error(error: serde_json::Error) -> CtlError {
    CtlError::message(format!("failed to serialize the application: {error}"))
}

fn read_document_file(path: &Path) -> Result<UserEditDocument, CtlError> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        CtlError::message(format!(
            "failed to read user document {}: {error}",
            path.display()
        ))
    })?;
    parse_document(&contents, &format!("user document {}", path.display()))
}

/// Write the current user as a source-of-truth JSON document, let the operator
/// edit it, and return the edited text.
fn edit_document(user: &User) -> Result<String, CtlError> {
    let document = serde_json::json!({
        "username": user.username,
        "email": user.email,
        "password": "",
        "admin": user.admin,
        "recovery_authentication_enabled": user.recovery_authentication_enabled,
        "external_authentication_uid": user.external_authentication_uid,
        "display_name": user.display_name,
        "first_name": user.first_name,
        "middle_name": user.middle_name,
        "last_name": user.last_name,
    });
    let mut contents = serde_json::to_string_pretty(&document)
        .map_err(|error| CtlError::message(format!("failed to serialize the user: {error}")))?;
    contents.push('\n');

    let mut file = tempfile::Builder::new()
        .prefix("microwave-user-edit-")
        .tempfile()
        .map_err(|error| {
            CtlError::message(format!(
                "failed to create a temporary user document: {error}"
            ))
        })?;
    restrict_permissions(&file)?;
    file.write_all(contents.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| {
            CtlError::message(format!(
                "failed to write the temporary user document: {error}"
            ))
        })?;

    let editor = editor_command();
    // The document path is passed as a positional argument instead of being
    // interpolated into the shell command so paths with spaces are safe.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file.path())
        .status()
        .map_err(|error| {
            CtlError::message(format!("failed to launch editor {editor:?}: {error}"))
        })?;
    if !status.success() {
        return Err(CtlError::message(format!(
            "editor {editor:?} exited with {}; the user was not changed",
            status
                .code()
                .map(|code| format!("status {code}"))
                .unwrap_or_else(|| "a signal".into())
        )));
    }

    std::fs::read_to_string(file.path()).map_err(|error| {
        CtlError::message(format!(
            "failed to read the edited user document {}: {error}",
            file.path().display()
        ))
    })
}

#[cfg(unix)]
fn restrict_permissions(file: &tempfile::NamedTempFile) -> Result<(), CtlError> {
    use std::os::unix::fs::PermissionsExt;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            CtlError::message(format!(
                "failed to restrict permissions on the temporary user document: {error}"
            ))
        })
}

#[cfg(not(unix))]
fn restrict_permissions(_file: &tempfile::NamedTempFile) -> Result<(), CtlError> {
    Ok(())
}

fn editor_command() -> String {
    for name in ["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "vi".into()
}

fn parse_document(contents: &str, description: &str) -> Result<UserEditDocument, CtlError> {
    let value: Value = serde_json::from_str(contents)
        .map_err(|error| CtlError::message(format!("invalid JSON in {description}: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| CtlError::message(format!("{description} must contain a JSON object")))?;

    for field in object.keys() {
        if !EDITABLE_FIELDS.contains(&field.as_str()) {
            return Err(CtlError::message(format!(
                "unknown field {field:?} in {description}; editable fields are: {}",
                EDITABLE_FIELDS.join(", ")
            )));
        }
    }

    Ok(UserEditDocument {
        username: optional_name(object.get("username"), description)?,
        email: optional_patch_string(object.get("email"), "email", description)?,
        password: optional_password(object.get("password"), description)?,
        admin: optional_bool(object.get("admin"), "admin", description)?,
        recovery_authentication_enabled: optional_bool(
            object.get("recovery_authentication_enabled"),
            "recovery_authentication_enabled",
            description,
        )?,
        external_authentication_uid: optional_patch_string(
            object.get("external_authentication_uid"),
            "external_authentication_uid",
            description,
        )?,
        display_name: optional_patch_string(
            object.get("display_name"),
            "display_name",
            description,
        )?,
        first_name: optional_patch_string(object.get("first_name"), "first_name", description)?,
        middle_name: optional_patch_string(object.get("middle_name"), "middle_name", description)?,
        last_name: optional_patch_string(object.get("last_name"), "last_name", description)?,
    })
}

fn optional_name(value: Option<&Value>, description: &str) -> Result<Option<String>, CtlError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Err(CtlError::message(format!(
            "username in {description} must not be empty"
        ))),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CtlError::message(format!(
            "username in {description} must be a string"
        ))),
    }
}

fn optional_patch_string(
    value: Option<&Value>,
    field: &str,
    description: &str,
) -> Result<Option<Option<String>>, CtlError> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => Ok(Some(Some(value.clone()))),
        Some(_) => Err(CtlError::message(format!(
            "{field} in {description} must be a string or null"
        ))),
    }
}

fn optional_password(value: Option<&Value>, description: &str) -> Result<Option<String>, CtlError> {
    match value {
        // An empty password (as written by the generated document) or an
        // explicit null leaves the stored password alone.
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CtlError::message(format!(
            "password in {description} must be a string"
        ))),
    }
}

fn optional_bool(
    value: Option<&Value>,
    field: &str,
    description: &str,
) -> Result<Option<bool>, CtlError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(CtlError::message(format!(
            "{field} in {description} must be a boolean"
        ))),
    }
}

fn build_patch(
    user: &User,
    document: &UserEditDocument,
) -> Result<(UserPatch, Vec<String>), CtlError> {
    let mut changed_fields: Vec<String> = Vec::new();

    let new_username = match &document.username {
        Some(username) if username != &user.username => {
            changed_fields.push("username".to_string());
            Some(username.clone())
        }
        _ => None,
    };

    let email = document.email.as_ref().and_then(|email| {
        let requested = email.clone().unwrap_or_default().to_lowercase();
        let current = user.email.clone().unwrap_or_default().to_lowercase();
        if requested == current {
            None
        } else {
            // `UserPatch::email` is set-only, so clearing stores an empty
            // string, which `user-show` and the API render as a missing email.
            changed_fields.push("email".to_string());
            Some(requested)
        }
    });

    let password = match &document.password {
        Some(password) if !password.is_empty() => {
            let hash = hash_password(password)?;
            changed_fields.push("password".to_string());
            Some((hash.hash, hash.salt, hash.hash_type))
        }
        _ => None,
    };

    if document.admin.is_some_and(|admin| admin != user.admin) {
        changed_fields.push("admin".to_string());
    }
    if document
        .recovery_authentication_enabled
        .is_some_and(|enabled| enabled != user.recovery_authentication_enabled)
    {
        changed_fields.push("recovery_authentication_enabled".to_string());
    }

    let profile = [
        (
            "external_authentication_uid",
            &document.external_authentication_uid,
            &user.external_authentication_uid,
        ),
        ("display_name", &document.display_name, &user.display_name),
        ("first_name", &document.first_name, &user.first_name),
        ("middle_name", &document.middle_name, &user.middle_name),
        ("last_name", &document.last_name, &user.last_name),
    ];
    for (field, requested, current) in profile {
        let changed = match requested {
            None => false,
            Some(Some(value)) => current.as_deref() != Some(value.as_str()),
            Some(None) => current.is_some(),
        };
        if changed {
            changed_fields.push(field.to_string());
        }
    }

    let patch = UserPatch {
        new_username,
        email,
        password,
        admin: document.admin,
        recovery_authentication_enabled: document.recovery_authentication_enabled,
        external_authentication_uid: document.external_authentication_uid.clone(),
        display_name: document.display_name.clone(),
        first_name: document.first_name.clone(),
        middle_name: document.middle_name.clone(),
        last_name: document.last_name.clone(),
    };
    Ok((patch, changed_fields))
}
