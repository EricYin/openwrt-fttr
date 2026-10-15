use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Serialize)]
pub struct Member {
    pub name: String,
    pub bytes: usize,
    pub encoding: &'static str,
    pub text: Option<String>,
    pub text_omitted: bool,
}

#[derive(Clone, Serialize)]
pub struct Field {
    pub value: serde_json::Value,
    pub member: String,
    pub unit: Option<&'static str>,
}

#[derive(Clone, Serialize)]
pub struct Report {
    pub schema_version: u8,
    pub compressed_bytes: usize,
    pub decompressed_bytes: usize,
    pub fields: BTreeMap<&'static str, Field>,
    pub members: Vec<Member>,
    pub errors: Vec<String>,
}
