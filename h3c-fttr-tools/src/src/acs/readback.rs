use serde::Deserialize;
use serde_json::value::RawValue;

pub(super) fn observed(state: &super::State, device: &str, prefix: &str) -> Option<bool> {
    state
        .tasks
        .values()
        .rev()
        .filter(|task| {
            task.device == device
                && task.method == "GetParameterValues"
                && task.state == super::TaskState::Completed
                && super::now().saturating_sub(
                    state
                        .results
                        .get(&task.id)
                        .and_then(|i| i.completed_at)
                        .unwrap_or(task.sent.unwrap_or(0)),
                ) <= 90
        })
        .find_map(|task| {
            state
                .results
                .get(&task.id)
                .and_then(|i| i.management_services.get(prefix))
                .copied()
                .or_else(|| is_management(task.result.as_deref()?, prefix))
        })
}

pub(super) fn service_evidence(
    value: &serde_json::Value,
) -> std::collections::BTreeMap<String, bool> {
    let mut evidence = std::collections::BTreeMap::new();
    for row in value["parameters"].as_array().into_iter().flatten() {
        let (Some(name), Some(value)) = (row["name"].as_str(), row["value"].as_str()) else {
            continue;
        };
        if name.len() > 512 || !(name.ends_with("ServiceList") || name.ends_with("SERVICELIST")) {
            continue;
        }
        let Some((prefix, _)) = name.rsplit_once('.') else {
            continue;
        };
        let management = crate::provisioning::contains_service_token(value, "TR069");
        evidence
            .entry(format!("{prefix}."))
            .and_modify(|v| *v |= management)
            .or_insert(management);
        if evidence.len() >= 64 {
            break;
        }
    }
    evidence
}

#[derive(Deserialize)]
struct Readback<'a> {
    #[serde(borrow)]
    parameters: Vec<Parameter<'a>>,
}

#[derive(Deserialize)]
struct Parameter<'a> {
    #[serde(borrow)]
    name: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    value: std::borrow::Cow<'a, str>,
}

pub(super) fn is_management(result: &RawValue, prefix: &str) -> Option<bool> {
    let readback: Readback<'_> = match serde_json::from_str(result.get()) {
        Ok(readback) => readback,
        Err(_) => return Some(true),
    };
    readback
        .parameters
        .iter()
        .find(|p| {
            p.name.starts_with(prefix)
                && (p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST"))
        })
        .map(|p| crate::provisioning::contains_service_token(&p.value, "TR069"))
}
