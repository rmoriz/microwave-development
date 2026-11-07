use microwave_config::{
    tenant_issues, ObjectStoreConfig, ObjectStoreOrgConfig, OrgCredentials, TenantIssue,
};
use std::collections::{BTreeMap, BTreeSet};

fn orgs(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn credentials(pairs: &[(&str, &str, &str)]) -> BTreeMap<String, OrgCredentials> {
    pairs
        .iter()
        .map(|(organization, access_key, secret_key)| {
            (
                (*organization).to_string(),
                OrgCredentials {
                    access_key: (*access_key).to_string(),
                    secret_key: (*secret_key).to_string(),
                },
            )
        })
        .collect()
}

fn config(entries: &[&str], require_org_bucket: bool) -> ObjectStoreConfig {
    ObjectStoreConfig {
        organizations: entries
            .iter()
            .map(|organization| {
                (
                    (*organization).to_string(),
                    ObjectStoreOrgConfig {
                        bucket: format!("mw-{organization}"),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        require_org_bucket,
        ..Default::default()
    }
}

#[test]
fn a_complete_map_reports_nothing() {
    let organizations = orgs(&["acme", "beta"]);
    let credentials = credentials(&[("acme", "a", "s"), ("beta", "b", "t")]);

    for require_org_bucket in [false, true] {
        assert_eq!(
            tenant_issues(
                &config(&["acme", "beta"], require_org_bucket),
                &organizations,
                &credentials
            ),
            Vec::new()
        );
    }
}

#[test]
fn a_missing_entry_is_only_reported_in_strict_mode() {
    let organizations = orgs(&["acme"]);
    let credentials = credentials(&[("acme", "a", "s")]);

    assert_eq!(
        tenant_issues(&config(&[], false), &organizations, &credentials),
        Vec::new()
    );
    assert_eq!(
        tenant_issues(&config(&[], true), &organizations, &credentials),
        vec![TenantIssue::MissingEntry("acme".into())]
    );
}

#[test]
fn missing_credentials_are_only_reported_in_strict_mode() {
    let organizations = orgs(&["acme", "beta"]);
    let credentials = credentials(&[("acme", "", "s"), ("beta", "b", "")]);

    assert_eq!(
        tenant_issues(
            &config(&["acme", "beta"], false),
            &organizations,
            &credentials
        ),
        Vec::new()
    );
    assert_eq!(
        tenant_issues(
            &config(&["acme", "beta"], true),
            &organizations,
            &credentials
        ),
        vec![
            TenantIssue::MissingCredential("acme".into(), "access_key"),
            TenantIssue::MissingCredential("beta".into(), "secret_key"),
        ]
    );
}

#[test]
fn an_organization_without_credentials_reports_both_fields() {
    let organizations = orgs(&["acme"]);

    assert_eq!(
        tenant_issues(&config(&["acme"], true), &organizations, &BTreeMap::new()),
        vec![
            TenantIssue::MissingCredential("acme".into(), "access_key"),
            TenantIssue::MissingCredential("acme".into(), "secret_key"),
        ]
    );
}

#[test]
fn an_entry_without_an_organization_is_reported_in_both_modes() {
    let organizations = orgs(&["acme"]);
    let credentials = credentials(&[("acme", "a", "s")]);
    let config = config(&["acme", "ghost"], false);

    for require_org_bucket in [false, true] {
        let config = ObjectStoreConfig {
            require_org_bucket,
            ..config.clone()
        };
        assert_eq!(
            tenant_issues(&config, &organizations, &credentials),
            vec![TenantIssue::UnknownOrg("ghost".into())]
        );
    }
}

#[test]
fn an_unknown_organization_is_not_a_missing_entry() {
    let organizations = orgs(&["acme"]);
    let credentials = credentials(&[("acme", "a", "s"), ("ghost", "g", "h")]);

    assert_eq!(
        tenant_issues(
            &config(&["acme", "ghost"], true),
            &organizations,
            &credentials
        ),
        vec![TenantIssue::UnknownOrg("ghost".into())]
    );
}

#[test]
fn the_issue_list_is_sorted() {
    let organizations = orgs(&["acme", "beta", "gamma"]);
    let credentials = credentials(&[("acme", "a", "s"), ("delta", "d", "e"), ("gamma", "g", "")]);
    // `beta` has an entry but no credentials, `ghost` is mapped but unknown,
    // and `delta` is known but unmapped.
    let organizations = {
        let mut organizations = organizations;
        organizations.insert("delta".into());
        organizations
    };

    assert_eq!(
        tenant_issues(
            &config(&["acme", "beta", "ghost", "gamma"], true),
            &organizations,
            &credentials
        ),
        vec![
            TenantIssue::UnknownOrg("ghost".into()),
            TenantIssue::MissingEntry("delta".into()),
            TenantIssue::MissingCredential("beta".into(), "access_key"),
            TenantIssue::MissingCredential("beta".into(), "secret_key"),
            TenantIssue::MissingCredential("gamma".into(), "secret_key"),
        ]
    );
}

#[test]
fn every_issue_names_the_organization_and_the_setting() {
    assert_eq!(
        TenantIssue::UnknownOrg("ghost".into()).to_string(),
        "object_store.organizations.ghost names an unknown organization"
    );
    assert_eq!(
        TenantIssue::MissingEntry("acme".into()).to_string(),
        "organization acme has no object_store.organizations entry"
    );
    assert_eq!(
        TenantIssue::MissingCredential("acme".into(), "access_key").to_string(),
        "organization acme is missing object_store.acme.access_key"
    );
}
