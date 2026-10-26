//! SQLite-backed metadata store for Microwave Server.

mod artifacts;
mod authz;
mod backend;
mod backend_sqlite;
mod compression;
mod cookbooks;
mod identity;
mod kv;
mod maintenance;
mod membership;
mod migration;
mod oauth;
mod objects;
mod policies;
mod sandboxes;
mod secrets;
mod sqlite;

pub use artifacts::{StoredCookbookArtifact, StoredCookbookArtifactVersion};
pub use authz::{Acl, AclEntry, Container, Group, GroupMembers, Permission};
pub use backend::{
    ArtifactRepository, AuthzRepository, CookbookRepository, HealthRepository, IdentityRepository,
    KvRepository, MembershipRepository, MetadataStore, OAuthRepository, ObjectRepository,
    PolicyRepository, SandboxRepository, SecretsRepository,
};
pub use compression::{
    decode_serialized, decode_serialized_optional, encode_serialized, encode_serialized_optional,
    Compression, COMPRESSED_PREFIX, MAX_DECOMPRESSED_BYTES,
};
pub use cookbooks::{parse_version, StoredCookbookVersion};
pub use identity::{ActorKey, ActorType, Client, Org, Principal, User, UserPatch, UserProfile};
pub use maintenance::MAINTENANCE_KV_KEY;
pub use membership::Invitation;
pub use migration::*;
pub use oauth::{OAuthAccessGrant, OAuthAccessToken, OAuthApplication, ID_SESSION_TTL_SECONDS};
pub use objects::{NewNode, StoredDataBagItem, StoredNode, StoredObject};
pub use policies::{
    StoredPolicy, StoredPolicyGroup, StoredPolicyGroupAssociation, StoredPolicyRevision,
};
pub use sandboxes::SandboxCheck;
pub use secrets::{
    secret_kv_key, BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY, BOOKSHELF_SECRET_GROUP,
    OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME,
};
pub use sqlite::{SqliteStore, StoreError};
