//! OC-ID compatible OAuth2 and identity surface.

mod applications;
mod csrf;
mod error;
mod oauth;
mod password_reset;
mod profile;
mod sessions;
mod state;
mod util;
mod v1;

pub use error::OAuthError;
pub use state::{OAuthState, ProvidesOAuthState};

use axum::routing::{delete, get, post, put};
use axum::Router;

/// Build the `/id` compatibility router.
pub fn routes<S>() -> Router<S>
where
    S: ProvidesOAuthState,
{
    Router::new()
        .route("/id", get(sessions::home))
        .route("/id/", get(sessions::home))
        .route("/id/signin", get(sessions::signin))
        .route("/id/sessions/new", get(sessions::signin))
        .route("/id/sessions", post(sessions::create_session))
        .route(
            "/id/sessions/{id}",
            delete(sessions::signout).post(sessions::signout),
        )
        .route("/id/auth/chef/callback", post(sessions::create_session))
        .route("/id/auth/failure", get(sessions::auth_failure))
        .route(
            "/id/signout",
            delete(sessions::signout).post(sessions::signout),
        )
        .route(
            "/id/oauth/authorized_applications",
            get(sessions::authorized_applications),
        )
        .route(
            "/id/oauth/authorized_applications/{id}",
            delete(sessions::revoke_authorized_application)
                .post(sessions::revoke_authorized_application),
        )
        .route(
            "/id/oauth/authorize",
            get(oauth::authorize)
                .post(oauth::authorize_approve)
                .delete(oauth::authorize_deny),
        )
        .route("/id/oauth/token", post(oauth::token))
        .route("/id/oauth/token/info", get(oauth::token_info))
        .route("/id/oauth/revoke", post(oauth::revoke))
        .route(
            "/id/oauth/applications/new",
            get(applications::new_application),
        )
        .route(
            "/id/oauth/applications",
            get(applications::list_applications).post(applications::create_application),
        )
        .route(
            "/id/oauth/applications/{id}",
            get(applications::get_application)
                .put(applications::update_application)
                .patch(applications::update_application)
                .post(applications::update_application)
                .delete(applications::delete_application),
        )
        .route(
            "/id/profile",
            get(profile::show)
                .put(profile::update)
                .patch(profile::update)
                .post(profile::update),
        )
        .route(
            "/id/profile/password",
            put(profile::change_password)
                .patch(profile::change_password)
                .post(profile::change_password),
        )
        .route("/id/profile/email", get(profile::email))
        .route("/id/profile/regen_key", post(profile::regen_key))
        .route("/id/password-reset/new", get(password_reset::show))
        .route(
            "/id/password-reset",
            get(password_reset::show)
                .post(password_reset::create)
                .put(password_reset::update)
                .patch(password_reset::update),
        )
        .route("/id/v1/me", get(v1::me))
        .route("/id/v1/me/organizations", get(v1::organizations))
        .route("/id/v1/users/{id}", get(v1::user))
        .route("/id/v1/status", get(v1::status))
        .layer(axum::middleware::from_fn(csrf::require_same_origin))
}
