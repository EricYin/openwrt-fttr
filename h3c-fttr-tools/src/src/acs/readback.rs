use serde::Deserialize;
use serde_json::value::RawValue;

pub(super) fn observed(state: &super::State, device: &str, prefix: &str) -> Option<bool> {
    state.tasks.values().rev().filter(|task| {
        task.device == device
            && task.method == "GetParameterValues"
            && task.state == super::TaskState::Completed
            && super::now().saturating_sub(task.sent.unwrap_or(0)) <= 90
    }).find_map(|task| is_management(task.result.as_deref()?, prefix))
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
    readback.parameters.iter().find(|p| {
        p.name.starts_with(prefix)
            && (p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST"))
    }).map(|p| crate::provisioning::contains_service_token(&p.value, "TR069"))
}
