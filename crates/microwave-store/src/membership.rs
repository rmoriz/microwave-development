use crate::identity::{new_id, now, user_from_row, Org, User};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

/// An outstanding request for a user to join an organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub id: String,
    pub org_id: String,
    pub org_name: String,
    pub user_id: String,
    pub username: String,
    pub created_at: String,
    pub updated_at: String,
}

const INVITATION_COLUMNS: &str =
    "i.id, i.org_id, o.name, i.user_id, u.username, i.created_at, i.updated_at";

const INVITATION_JOINS: &str = "FROM org_user_invites i
     JOIN orgs o ON o.id = i.org_id
     JOIN users u ON u.id = i.user_id";

impl SqliteStore {
    /// Users associated with an organization, ordered by username.
    pub fn list_org_users(&self, org_id: &str) -> Result<Vec<User>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT u.id, u.authz_id, u.username, u.email, u.password_hash, u.password_salt,
                    u.hash_type, u.admin, u.recovery_authentication_enabled,
                    u.external_authentication_uid, u.display_name, u.first_name,
                    u.middle_name, u.last_name, u.created_at, u.updated_at
             FROM users u
             JOIN org_user_associations a ON a.user_id = u.id
             WHERE a.org_id = ?1
             ORDER BY u.username",
        )?;
        let rows = statement.query_map([org_id], user_from_row)?;
        let mut users = Vec::new();
        for row in rows {
            users.push(row?);
        }
        Ok(users)
    }

    pub fn is_user_in_org(&self, org_id: &str, user_id: &str) -> Result<bool, StoreError> {
        let connection = self.connection();
        let exists: i64 = connection.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM org_user_associations WHERE org_id = ?1 AND user_id = ?2
             )",
            params![org_id, user_id],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    /// Associate a user with an organization and add them to its `users`
    /// group in a single transaction.
    pub fn add_user_to_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let users_group = org_group(&transaction, org_id, "users")?;

        transaction
            .execute(
                "INSERT INTO org_user_associations
                 (org_id, user_id, last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, '', ?3, ?3)",
                params![org_id, user_id, now()],
            )
            .map_err(|error| {
                crate::identity::conflict(error, "The association already exists.".to_string())
            })?;

        add_group_actor(&transaction, &users_group, user_id)?;
        let user_authz_id: String = transaction.query_row(
            "SELECT authz_id FROM users WHERE id = ?1",
            [user_id],
            |row| row.get(0),
        )?;
        crate::authz::add_org_read_access_ace(&transaction, org_id, &user_authz_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Remove an organization association and its `users` group membership.
    ///
    /// Mirrors the upstream policy: a user who is still in the `admins` group
    /// must be removed from that group first.
    pub fn remove_user_from_org(
        &self,
        org_id: &str,
        user_id: &str,
        username: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let users_group = org_group(&transaction, org_id, "users")?;
        let admins_group = org_group(&transaction, org_id, "admins")?;
        let user_authz_id: String = transaction
            .query_row(
                "SELECT authz_id FROM users WHERE id = ?1",
                [user_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("user {username}")))?;

        if actor_in_group(&transaction, &admins_group, &user_authz_id)? {
            return Err(StoreError::Forbidden(format!(
                "Please remove {username} from this organization's admins group before removing him or her from the organization."
            )));
        }

        let removed = transaction.execute(
            "DELETE FROM org_user_associations WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
        )?;
        if removed == 0 {
            return Err(StoreError::NotFound(format!(
                "association for user {username}"
            )));
        }
        transaction.execute(
            "DELETE FROM authz_group_actors
             WHERE group_authz_id = ?1 AND actor_authz_id = ?2",
            params![users_group, user_authz_id],
        )?;
        crate::authz::remove_org_read_access_ace(&transaction, org_id, &user_authz_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Organizations the user belongs to, ordered by name.
    pub fn list_user_orgs(&self, user_id: &str) -> Result<Vec<Org>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT o.id, o.authz_id, o.name, o.full_name, o.created_at, o.updated_at
             FROM orgs o
             JOIN org_user_associations a ON a.org_id = o.id
             WHERE a.user_id = ?1
             ORDER BY o.name",
        )?;
        let rows = statement.query_map([user_id], |row| {
            Ok(Org {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                name: row.get(2)?,
                full_name: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        let mut orgs = Vec::new();
        for row in rows {
            orgs.push(row?);
        }
        Ok(orgs)
    }

    /// Whether an actor is a member of a group, following child groups.
    pub fn user_in_group(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection();
        actor_in_group(&connection, group_authz_id, actor_authz_id)
    }

    /// Whether `actor_authz_id` is an admin of any organization the given user
    /// belongs to.
    pub fn is_org_admin_for_user(
        &self,
        actor_authz_id: &str,
        user_id: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection();
        let exists: i64 = connection.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM org_user_associations association
                 JOIN groups admins
                   ON admins.org_id = association.org_id
                  AND admins.name = 'admins'
                 WHERE association.user_id = ?2
                   AND EXISTS (
                       WITH RECURSIVE actor_groups(group_authz_id) AS (
                           SELECT group_authz_id
                           FROM authz_group_actors
                           WHERE actor_authz_id = ?1
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
            params![actor_authz_id, user_id],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn create_invitation(
        &self,
        org_id: &str,
        user_id: &str,
        last_updated_by: &str,
    ) -> Result<Invitation, StoreError> {
        let id = new_id();
        let timestamp = now();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO org_user_invites
                 (id, org_id, user_id, last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, org_id, user_id, last_updated_by, timestamp],
            )
            .map_err(|error| {
                crate::identity::conflict(error, "The invite already exists.".to_string())
            })?;
        drop(connection);
        self.find_invitation(&id)?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))
    }

    pub fn find_invitation(&self, id: &str) -> Result<Option<Invitation>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!("SELECT {INVITATION_COLUMNS} {INVITATION_JOINS} WHERE i.id = ?1"),
                [id],
                invitation_from_row,
            )
            .optional()?)
    }

    pub fn list_org_invitations(&self, org_id: &str) -> Result<Vec<Invitation>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS}
             WHERE i.org_id = ?1 ORDER BY u.username"
        ))?;
        let rows = statement.query_map([org_id], invitation_from_row)?;
        let mut invitations = Vec::new();
        for row in rows {
            invitations.push(row?);
        }
        Ok(invitations)
    }

    pub fn list_user_invitations(&self, user_id: &str) -> Result<Vec<Invitation>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {INVITATION_COLUMNS} {INVITATION_JOINS}
             WHERE i.user_id = ?1 ORDER BY o.name"
        ))?;
        let rows = statement.query_map([user_id], invitation_from_row)?;
        let mut invitations = Vec::new();
        for row in rows {
            invitations.push(row?);
        }
        Ok(invitations)
    }

    pub fn count_user_invitations(&self, user_id: &str) -> Result<usize, StoreError> {
        let connection = self.connection();
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM org_user_invites WHERE user_id = ?1",
            [user_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn delete_invitation(&self, id: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        let removed = connection.execute("DELETE FROM org_user_invites WHERE id = ?1", [id])?;
        if removed == 0 {
            return Err(StoreError::NotFound(format!("association request {id}")));
        }
        Ok(())
    }

    /// Accept an invitation: associate the user, add them to the organization's
    /// `users` group, and consume the invitation.
    pub fn accept_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let invitation = invitation_by_id(&transaction, id)?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))?;
        let users_group = org_group(&transaction, &invitation.org_id, "users")?;
        let timestamp = now();

        transaction.execute(
            "INSERT OR IGNORE INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, '', ?3, ?3)",
            params![invitation.org_id, invitation.user_id, timestamp],
        )?;
        add_group_actor(&transaction, &users_group, &invitation.user_id)?;
        let user_authz_id: String = transaction.query_row(
            "SELECT authz_id FROM users WHERE id = ?1",
            [&invitation.user_id],
            |row| row.get(0),
        )?;
        crate::authz::add_org_read_access_ace(&transaction, &invitation.org_id, &user_authz_id)?;
        transaction.execute("DELETE FROM org_user_invites WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(invitation)
    }

    /// Reject an invitation without creating an organization association.
    pub fn reject_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let invitation = invitation_by_id(&transaction, id)?
            .ok_or_else(|| StoreError::NotFound(format!("association request {id}")))?;
        transaction.execute("DELETE FROM org_user_invites WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(invitation)
    }
}

fn org_group(connection: &Connection, org_id: &str, name: &str) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT authz_id FROM groups WHERE org_id = ?1 AND name = ?2",
            params![org_id, name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("group {name}")))
}

fn add_group_actor(
    connection: &Connection,
    group_authz_id: &str,
    actor_id: &str,
) -> Result<(), StoreError> {
    // Group membership is keyed by authz id; accept either an actor id or an
    // authz id so callers can pass the identifiers they already hold.
    let actor_authz_id: Option<String> = connection
        .query_row(
            "SELECT authz_id FROM users WHERE id = ?1
             UNION ALL
             SELECT authz_id FROM clients WHERE id = ?1
             LIMIT 1",
            [actor_id],
            |row| row.get(0),
        )
        .optional()?;
    let actor_authz_id = actor_authz_id.unwrap_or_else(|| actor_id.to_string());
    connection.execute(
        "INSERT OR IGNORE INTO authz_group_actors (group_authz_id, actor_authz_id)
         VALUES (?1, ?2)",
        params![group_authz_id, actor_authz_id],
    )?;
    Ok(())
}

fn actor_in_group(
    connection: &Connection,
    group_authz_id: &str,
    actor_authz_id: &str,
) -> Result<bool, StoreError> {
    let exists: i64 = connection.query_row(
        "WITH RECURSIVE members(group_authz_id) AS (
             SELECT group_authz_id
             FROM authz_group_actors
             WHERE actor_authz_id = ?1
             UNION
             SELECT relation.parent_group_authz_id
             FROM authz_group_groups relation
             JOIN members ON relation.child_group_authz_id = members.group_authz_id
         )
         SELECT EXISTS (SELECT 1 FROM members WHERE group_authz_id = ?2)",
        params![actor_authz_id, group_authz_id],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

fn invitation_by_id(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<Option<Invitation>, StoreError> {
    Ok(transaction
        .query_row(
            &format!("SELECT {INVITATION_COLUMNS} {INVITATION_JOINS} WHERE i.id = ?1"),
            [id],
            invitation_from_row,
        )
        .optional()?)
}

fn invitation_from_row(row: &Row<'_>) -> rusqlite::Result<Invitation> {
    Ok(Invitation {
        id: row.get(0)?,
        org_id: row.get(1)?,
        org_name: row.get(2)?,
        user_id: row.get(3)?,
        username: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
