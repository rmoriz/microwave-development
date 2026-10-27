//! Extraction of Chef `run_converge` payloads for local report storage.

use crate::ApiError;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

pub(crate) struct PreparedNodeRun {
    pub node_name: String,
    pub run_id: String,
    pub status: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub resource_count: i64,
    pub updated_resource_count: i64,
    pub error_count: i64,
    pub report: String,
}

pub(crate) fn prepare_node_run(body: &[u8]) -> Result<Option<PreparedNodeRun>, ApiError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ApiError::bad_request("Invalid JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;
    if object.get("message_type").and_then(Value::as_str) != Some("run_converge") {
        return Ok(None);
    }

    let node_name = required_string(object.get("node_name"), "node_name")?;
    let run_id = required_string(object.get("run_id"), "run_id")?;
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string();

    let start_time = normalize_timestamp(object.get("start_time"))
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
    let end_time = normalize_timestamp(object.get("end_time"));

    let resources = object.get("resources").and_then(Value::as_array);
    let resource_count = integer(object.get("total_resource_count"))
        .or_else(|| resources.map(|items| items.len() as i64))
        .unwrap_or(0)
        .max(0);
    let updated_resource_count = integer(object.get("updated_resource_count"))
        .or_else(|| {
            resources.map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("status").and_then(Value::as_str) == Some("updated"))
                    .count() as i64
            })
        })
        .unwrap_or(0)
        .max(0);
    let error_count = (object.get("error").is_some_and(|value| !value.is_null()) as i64)
        + resources
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("error").is_some_and(|value| !value.is_null()))
                    .count() as i64
            })
            .unwrap_or(0);

    let report = std::str::from_utf8(body)
        .map_err(|_| ApiError::bad_request("Invalid JSON"))?
        .to_string();

    Ok(Some(PreparedNodeRun {
        node_name,
        run_id,
        status,
        start_time,
        end_time,
        resource_count,
        updated_resource_count,
        error_count,
        report,
    }))
}

fn required_string(value: Option<&Value>, field: &str) -> Result<String, ApiError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("Field '{field}' missing")))
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn normalize_timestamp(value: Option<&Value>) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(value?.as_str()?).ok()?;
    Some(
        parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &[u8] = br#"{
      "message_type":"run_converge",
      "node_name":"web1",
      "run_id":"abc",
      "status":"failure",
      "start_time":"2026-10-27T10:00:00+02:00",
      "end_time":"2026-10-27T10:05:00+02:00",
      "total_resource_count":4,
      "updated_resource_count":2,
      "error":{"class":"RuntimeError","message":"boom"},
      "unknown_field":42,
      "resources":[
        {"status":"updated","error":null},
        {"status":"failed","error":{"class":"Errno::ENOENT","message":"missing"}}
      ]
    }"#;

    #[test]
    fn parses_a_converged_run() {
        let run = prepare_node_run(FULL).unwrap().unwrap();
        assert_eq!(run.node_name, "web1");
        assert_eq!(run.run_id, "abc");
        assert_eq!(run.status, "failure");
        assert_eq!(run.start_time, "2026-10-27T08:00:00.000Z");
        assert_eq!(run.end_time.as_deref(), Some("2026-10-27T08:05:00.000Z"));
        assert_eq!(run.resource_count, 4);
        assert_eq!(run.updated_resource_count, 2);
        assert_eq!(run.error_count, 2);
        assert!(run.report.contains("\"unknown_field\":42"));
    }

    #[test]
    fn ignores_non_converge_and_missing_message_types() {
        assert!(prepare_node_run(br#"{"message_type":"run_start"}"#)
            .unwrap()
            .is_none());
        assert!(prepare_node_run(br#"{"message_type":"action"}"#)
            .unwrap()
            .is_none());
        assert!(prepare_node_run(br#"{"node_name":"web1"}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn derives_counts_and_unknown_status() {
        let run = prepare_node_run(
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1",
                 "resources":[{"status":"updated"},{"status":"updated"},{"status":"skipped"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(run.status, "unknown");
        assert_eq!(run.resource_count, 3);
        assert_eq!(run.updated_resource_count, 2);
        assert_eq!(run.error_count, 0);
    }

    #[test]
    fn falls_back_to_receive_time_for_missing_or_invalid_start() {
        for body in [
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1","start_time":"nope"}"#
                .as_slice(),
        ] {
            let run = prepare_node_run(body).unwrap().unwrap();
            assert!(chrono::DateTime::parse_from_rfc3339(&run.start_time).is_ok());
            assert!(run.start_time.ends_with('Z'));
            assert_eq!(run.start_time.len(), 24);
        }
    }

    #[test]
    fn normalizes_an_invalid_end_time_to_none() {
        let run = prepare_node_run(
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1","end_time":"nope"}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(run.end_time, None);
    }

    #[test]
    fn rejects_missing_identity_and_invalid_json() {
        for body in [
            br#"{"message_type":"run_converge","run_id":"1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"web1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"","run_id":"1"}"#.as_slice(),
            b"not-json".as_slice(),
        ] {
            assert!(prepare_node_run(body).is_err(), "body should be rejected");
        }
    }
}
