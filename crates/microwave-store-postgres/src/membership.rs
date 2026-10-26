use async_trait::async_trait;
use microwave_store::{Invitation, MembershipRepository, Org, StoreError, User};
use sqlx::postgres::{PgRow, Postgres};
use sqlx::{Row, Transaction};

use crate::identity::user_from_row;
use crate::store::{db, new_id, now, unique, PostgresStore};

const INVITATION_COLUMNS: &str =
    "i.id, i.org_id, o.name, i.user_id, u.username, i.created_at, i.updated_at";

const INVITATION_JOINS: &str = "FROM org_user_invites i
     JOIN orgs o ON o.id = i.org_id
     JOIN users u ON u.id = i.user_id";

fn invitation_from_row(row: &PgRow) -> Result<Invitation, sqlx::Error> {
    Ok(Invitation {
        id: row.try_get(0)?,
        org_id: row.try_get(1)?,
        org_name: row.try_get(2)?,
        user_id: row.try_get(3)?,
        username: row.try_get(4)?,
        created_at: row.try_get(5)?,
        updated_at: row.try_get(6)?,
    })
}

#[async_trait]
impl MembershipRepository for PostgresStore {
    async fn list_org_users(&self, org_id: &str) -> Result<Vec<User>, StoreError> {
        let rows = sqlx::query(
            "SELECT u.id, u.authz_id, u.username, u.email, u.password_hash, u.password_salt,
                    u.hash_type, u.admin, u.recovery_authentication_enabled,
                    u.external_authentication_uid, u.display_name, u.first_name,
                    u.middle_name, u.last_name, u.created_at, u.updated_at
             FROM users u
             JOIN org_user_associations a ON a.user_id = u.id
             WHERE a.org_id = $1
             ORDER BY u.username",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(user_from_row)
            .collect::<Result<_, _>>()
            .map_err(db)
    }

    async fn is_user_in_org(&self, org_id: &str, user_id: &str) -> Result<bool, StoreError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM org_user_associations WHERE org_id = $1 AND user_id = $2
             )",
        )
        .bind(org_id)
        .bind(user_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(exists)
    }

    async fn add_user_to_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let users_group = org_group(&mut transaction, org_id, "users").await?;
        sqlx::query(
            "INSERT INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, '', $3, $3)",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(now())
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, "The association already exists."))?;
        let user_authz_id: String = sqlx::query_scalar("SELECT authz_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
        ensure_user_specific_group(
            &mut transaction,
            org_id,
            user_id,
            &user_authz_id,
            &users_group,
        )
        .await?;
        crate::authz::add_org_read_access_ace(&mut transaction, org_id, &user_authz_id).await?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn remove_user_from_org(
        &self,
        org_id: &str,
        user_id: &str,
        username: &str,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let users_group = org_group(&mut transaction, org_id, "users").await?;
        let admins_group = org_group(&mut transaction, org_id, "admins").await?;
        let user_authz_id: Option<String> =
            sqlx::query_scalar("SELECT authz_id FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?;
        let user_authz_id =
            user_authz_id.ok_or_else(|| StoreError::NotFound(format!("user {username}")))?;

        if actor_in_group(&mut transaction, &admins_group, &user_authz_id).await? {
            return Err(StoreError::Forbidden(format!(
                "Please remove {username} from this organization's admins group before removing him or her from the organization."
            )));
        }

        let removed =
            sqlx::query("DELETE FROM org_user_associations WHERE org_id = $1 AND user_id = $2")
                .bind(org_id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
        if removed.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!(
                "association for user {username}"
            )));
        }
        sqlx::query(
            "DELETE FROM authz_group_actors
             WHERE group_authz_id = $1 AND actor_authz_id = $2",
        )
        .bind(&users_group)
        .bind(&user_authz_id)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        delete_user_specific_group(&mut transaction, org_id, user_id).await?;
        crate::authz::remove_org_read_access_ace(&mut transaction, org_id, &user_authz_id).await?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn list_user_orgs(&self, user_id: &str) -> Result<Vec<Org>, StoreError> {
        let rows = sqlx::query(
            "SELECT o.id, o.authz_id, o.name, o.full_name, o.created_at, o.updated_at
             FROM orgs o
             JOIN org_user_associations a ON a.org_id = o.id
             WHERE a.user_id = $1
             ORDER BY o.name",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(Org {
                    id: row.try_get(0)?,
                    authz_id: row.try_get(1)?,
                    name: row.try_get(2)?,
                    full_name: row.try_get(3)?,
                    created_at: row.try_get(4)?,
                    updated_at: row.try_get(5)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn user_in_group(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<bool, StoreError> {
        let exists: bool = sqlx::query_scalar(
            "WITH RECURSIVE members(group_authz_id) AS (
                 SELECT group_authz_id
                 FROM authz_group_actors
                 WHERE actor_authz_id = $1
                 UNION
                 SELECT relation.parent_group_authz_id
                 FROM authz_group_groups relation
                 JOIN members ON relation.child_group_authz_id = members.group_authz_id
             )
             SELECT EXISTS (SELECT 1 FROM members WHERE group_authz_id = $2)",
        )
        .bind(actor_authz_id)
        .bind(group_authz_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(exists)
    }

    async fn is_org_admin_for_user(
        &self,
        actor_authz_id: &str,
        user_id: &str,
    ) -> Result<bool, StoreError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                 FROM org_user_associations association
                 JOIN groups admins
                   ON admins.org_id = association.org_id
                  AND admins.name = 'admins'
                 WHERE association.user_id = $2
                   AND EXISTS (
                       WITH RECURSIVE actor_groups(group_authz_id) AS (
                           SELECT group_authz_id
                           FROM authz_group_actors
                           WHERE actor_authz_id = $1
                           UNION
                           SELECT relation.parent_group_authz_id
                           FROM authz_group_groups relation
                           JOIN actor_groups
                             ON relation.child_group_authz_id = actor_groups.group_authz_id
                       )
                       SELECT 1 FROM actor_groups
                       WHERE group_authz_id = admins.authz_id
                   )
             )",
        )
        .bind(actor_authz_id)
        .bind(user_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(exists)
    }

    async fn create_invitation(
        &self,
        org_id: &str,
        user_id: &str,
        last_updated_by: &str,
    ) -> Result<Invitation, StoreError> {
        let id = new_id();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO org_user_invites
             (id, org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(user_id)
        .bind(last_updated_by)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| unique(error, "The invite already exists."))?;
        self.find_invitation(&id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))
    }

    async fn find_invitation(&self, id: &str) -> Result<Option<Invitation>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS} WHERE i.id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| invitation_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_org_invitations(&self, org_id: &str) -> Result<Vec<Invitation>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS}
             WHERE i.org_id = $1 ORDER BY u.username"
        )))
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(invitation_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn list_user_invitations(&self, user_id: &str) -> Result<Vec<Invitation>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS}
             WHERE i.user_id = $1 ORDER BY o.name"
        )))
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(invitation_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn count_user_invitations(&self, user_id: &str) -> Result<usize, StoreError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM org_user_invites WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(self.pool())
                .await
                .map_err(db)?;
        Ok(count as usize)
    }

    async fn delete_invitation(&self, id: &str) -> Result<(), StoreError> {
        let removed = sqlx::query("DELETE FROM org_user_invites WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        if removed.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("association request {id}")));
        }
        Ok(())
    }

    async fn accept_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let invitation = invitation_by_id(&mut transaction, id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))?;
        let users_group = org_group(&mut transaction, &invitation.org_id, "users").await?;
        sqlx::query(
            "INSERT INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, '', $3, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(&invitation.org_id)
        .bind(&invitation.user_id)
        .bind(now())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let user_authz_id: String = sqlx::query_scalar("SELECT authz_id FROM users WHERE id = $1")
            .bind(&invitation.user_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
        ensure_user_specific_group(
            &mut transaction,
            &invitation.org_id,
            &invitation.user_id,
            &user_authz_id,
            &users_group,
        )
        .await?;
        crate::authz::add_org_read_access_ace(&mut transaction, &invitation.org_id, &user_authz_id)
            .await?;
        sqlx::query("DELETE FROM org_user_invites WHERE id = $1")
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(invitation)
    }

    async fn reject_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let invitation = invitation_by_id(&mut transaction, id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))?;
        sqlx::query("DELETE FROM org_user_invites WHERE id = $1")
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(invitation)
    }
}

async fn org_group(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    name: &str,
) -> Result<String, StoreError> {
    sqlx::query_scalar("SELECT authz_id FROM groups WHERE org_id = $1 AND name = $2")
        .bind(org_id)
        .bind(name)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound(format!("group {name}")))
}

/// Create the user-specific actor group (USAG) for an organization member.
///
/// Mirrors the SQLite membership implementation: the group is named after the
/// user's id, contains the user, and is a child of the organization's `users`
/// group.
async fn ensure_user_specific_group(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    user_id: &str,
    user_authz_id: &str,
    users_group_authz_id: &str,
) -> Result<String, StoreError> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT authz_id FROM groups WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(db)?;
    let usag_authz_id = match existing {
        Some(authz_id) => authz_id,
        None => {
            let id = new_id();
            let authz_id = new_id();
            sqlx::query(
                "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
                 VALUES ($1, 'group', $2, $3)",
            )
            .bind(&authz_id)
            .bind(org_id)
            .bind(now())
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
            sqlx::query(
                "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $5)",
            )
            .bind(&id)
            .bind(org_id)
            .bind(&authz_id)
            .bind(user_id)
            .bind(now())
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
            authz_id
        }
    };

    sqlx::query(
        "INSERT INTO authz_group_actors (group_authz_id, actor_authz_id)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(&usag_authz_id)
    .bind(user_authz_id)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO authz_group_groups (parent_group_authz_id, child_group_authz_id)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(users_group_authz_id)
    .bind(&usag_authz_id)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(usag_authz_id)
}

async fn delete_user_specific_group(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    user_id: &str,
) -> Result<(), StoreError> {
    let authz_id: Option<String> =
        sqlx::query_scalar("SELECT authz_id FROM groups WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(db)?;
    let Some(authz_id) = authz_id else {
        return Ok(());
    };

    for statement in [
        "DELETE FROM authz_acl_actor
         WHERE target_authz_id = $1 OR actor_authz_id = $1",
        "DELETE FROM authz_acl_group
         WHERE target_authz_id = $1 OR group_authz_id = $1",
        "DELETE FROM authz_group_actors
         WHERE group_authz_id = $1 OR actor_authz_id = $1",
        "DELETE FROM authz_group_groups
         WHERE parent_group_authz_id = $1 OR child_group_authz_id = $1",
    ] {
        sqlx::query(statement)
            .bind(&authz_id)
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
    }
    sqlx::query("DELETE FROM groups WHERE authz_id = $1")
        .bind(&authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
        .bind(&authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    Ok(())
}

async fn actor_in_group(
    transaction: &mut Transaction<'_, Postgres>,
    group_authz_id: &str,
    actor_authz_id: &str,
) -> Result<bool, StoreError> {
    let exists: bool = sqlx::query_scalar(
        "WITH RECURSIVE members(group_authz_id) AS (
             SELECT group_authz_id
             FROM authz_group_actors
             WHERE actor_authz_id = $1
             UNION
             SELECT relation.parent_group_authz_id
             FROM authz_group_groups relation
             JOIN members ON relation.child_group_authz_id = members.group_authz_id
         )
         SELECT EXISTS (SELECT 1 FROM members WHERE group_authz_id = $2)",
    )
    .bind(actor_authz_id)
    .bind(group_authz_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(exists)
}

async fn invitation_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<Option<Invitation>, StoreError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS} WHERE i.id = $1"
    )))
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    row.map(|row| invitation_from_row(&row))
        .transpose()
        .map_err(db)
}
