use serde::{Deserialize, Serialize};
use std::io;

pub const SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter { pub name: String, pub value: String, #[serde(default = "string_type")] pub kind: String }
fn string_type() -> String { "string".into() }
#[derive(Debug)]
pub struct Message {
    pub method: String, pub namespace: String, pub id: String,
    pub serial: String, pub oui: String, pub product: String,
    pub data: serde_json::Value,
}
fn bad(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
pub fn parse(input: &str) -> io::Result<Message> {
    let doc = roxmltree::Document::parse_with_options(input, roxmltree::ParsingOptions {
        allow_dtd: false, nodes_limit: 30_000, ..Default::default()
    }).map_err(|_| bad("invalid SOAP XML"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Envelope" || root.tag_name().namespace() != Some(SOAP) { return Err(bad("not SOAP 1.1")); }
    let body = root.children().find(|n| n.has_tag_name((SOAP, "Body"))).ok_or_else(|| bad("no SOAP Body"))?;
    let rpc = body.children().find(|n| n.is_element()).ok_or_else(|| bad("empty SOAP Body"))?;
    let namespace = if rpc.has_tag_name((SOAP, "Fault")) {
        rpc.descendants().find_map(|n| n.tag_name().namespace().filter(|s| s.starts_with("urn:dslforum-org:cwmp-"))).unwrap_or("urn:dslforum-org:cwmp-1-0")
    } else { rpc.tag_name().namespace().unwrap_or("") };
    if !matches!(namespace, "urn:dslforum-org:cwmp-1-0" | "urn:dslforum-org:cwmp-1-1" | "urn:dslforum-org:cwmp-1-2" | "urn:dslforum-org:cwmp-1-3" | "urn:dslforum-org:cwmp-1-4") { return Err(bad("unsupported CWMP namespace")); }
    let text = |name: &str| rpc.descendants().find(|n| n.is_element() && n.tag_name().name() == name).and_then(|n| n.text()).unwrap_or("").to_owned();
    let id = root.children().find(|n| n.has_tag_name((SOAP, "Header"))).and_then(|header| header.children().find(|n| n.tag_name().name() == "ID" && n.tag_name().namespace().is_some_and(|s| s.starts_with("urn:dslforum-org:cwmp-")))).and_then(|n| n.text()).unwrap_or("").to_string();
    let mut values = Vec::new();
    for node in rpc.descendants().filter(|n| n.is_element() && matches!(n.tag_name().name(), "ParameterValueStruct" | "ParameterInfoStruct" | "SetParameterValuesFault")) {
        let child = |name| node.children().find(|n| n.is_element() && n.tag_name().name() == name);
        let value = |name| child(name).and_then(|n| n.text()).unwrap_or("").to_owned();
        values.push(serde_json::json!({"name": if node.tag_name().name() == "SetParameterValuesFault" {value("ParameterName")} else {value("Name")}, "value": value("Value"), "writable": value("Writable"), "type": child("Value").and_then(|n| n.attribute(("http://www.w3.org/2001/XMLSchema-instance", "type"))).unwrap_or(""), "fault_code": value("FaultCode"), "fault_string": value("FaultString")}));
    }
    Ok(Message { method: rpc.tag_name().name().into(), namespace: namespace.into(), id,
        serial: text("SerialNumber"), oui: text("OUI"), product: text("ProductClass"),
        data: serde_json::json!({"parameters": values, "methods": rpc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "string").filter_map(|n| n.text()).collect::<Vec<_>>(), "status": text("Status"), "instance_number":text("InstanceNumber"), "fault_code": text("FaultCode"), "fault_string": text("FaultString"), "events": rpc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "EventCode").filter_map(|n| n.text()).collect::<Vec<_>>()}) })
}
pub fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}
pub fn envelope(namespace: &str, id: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\"?><soap:Envelope xmlns:soap=\"{SOAP}\" xmlns:cwmp=\"{}\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" xmlns:soap-enc=\"http://schemas.xmlsoap.org/soap/encoding/\"><soap:Header><cwmp:ID soap:mustUnderstand=\"1\">{}</cwmp:ID></soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>", escape(namespace), escape(id))
}
pub fn request(namespace: &str, id: u64, method: &str, args: &serde_json::Value) -> io::Result<String> {
    let body = match method {
        "GetRPCMethods" => "<cwmp:GetRPCMethods/>".into(),
        "GetParameterNames" => format!("<cwmp:GetParameterNames><ParameterPath>{}</ParameterPath><NextLevel>{}</NextLevel></cwmp:GetParameterNames>", escape(args["path"].as_str().unwrap_or("")), args["next_level"].as_bool().unwrap_or(false)),
        "GetParameterValues" => {
            let names = args["names"].as_array().ok_or_else(|| bad("names array required"))?;
            format!("<cwmp:GetParameterValues><ParameterNames soap-enc:arrayType=\"xsd:string[{}]\">{}</ParameterNames></cwmp:GetParameterValues>", names.len(), names.iter().map(|n| format!("<string>{}</string>", escape(n.as_str().unwrap_or("")))).collect::<String>())
        }
        "SetParameterValues" => {
            let values: Vec<Parameter> = serde_json::from_value(args["parameters"].clone()).map_err(|_| bad("parameters array required"))?;
            let mut list = String::new();
            for p in &values {
                if !matches!(p.kind.as_str(), "string" | "boolean" | "int" | "unsignedInt" | "dateTime" | "base64" | "hexBinary" | "long" | "unsignedLong") { return Err(bad("unsupported XSD type")); }
                list.push_str(&format!("<ParameterValueStruct><Name>{}</Name><Value xsi:type=\"xsd:{}\">{}</Value></ParameterValueStruct>", escape(&p.name), p.kind, escape(&p.value)));
            }
            format!("<cwmp:SetParameterValues><ParameterList soap-enc:arrayType=\"cwmp:ParameterValueStruct[{}]\">{list}</ParameterList><ParameterKey>fttr-{id}</ParameterKey></cwmp:SetParameterValues>", values.len())
        }
        "AddObject" | "DeleteObject" => format!("<cwmp:{method}><ObjectName>{}</ObjectName><ParameterKey>fttr-{id}</ParameterKey></cwmp:{method}>",escape(args["object"].as_str().unwrap_or(""))),
        _ => return Err(bad("unsupported ACS RPC")),
    };
    Ok(envelope(namespace, &format!("fttr-{id}"), &body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn object_response_and_parameter_fault() {
        let reply=parse(&envelope("urn:dslforum-org:cwmp-1-0","fttr-2","<cwmp:AddObjectResponse><InstanceNumber>4</InstanceNumber><Status>1</Status></cwmp:AddObjectResponse>")).unwrap();
        assert_eq!(reply.data["instance_number"],"4"); assert_eq!(reply.data["status"],"1");
        let fault=parse(&envelope("urn:dslforum-org:cwmp-1-0","fttr-2","<soap:Fault><detail><cwmp:Fault><FaultCode>9003</FaultCode><SetParameterValuesFault><ParameterName>InternetGatewayDevice.x</ParameterName><FaultCode>9005</FaultCode></SetParameterValuesFault></cwmp:Fault></detail></soap:Fault>")).unwrap();
        assert_eq!(fault.data["parameters"][0]["name"],"InternetGatewayDevice.x");
        let xml=request("urn:dslforum-org:cwmp-1-0",2,"DeleteObject",&serde_json::json!({"object":"InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4."})).unwrap();
        assert_eq!(parse(&xml).unwrap().method,"DeleteObject");
    }
    #[test] fn namespaces_entities_and_faults() {
        let input = envelope("urn:dslforum-org:cwmp-1-2", "id<&", "<cwmp:Inform><DeviceId><OUI>AABBCC</OUI><SerialNumber>SN&amp;1</SerialNumber></DeviceId><ParameterList><ParameterValueStruct><Name>x</Name><Value xsi:type=\"xsd:string\">a&lt;b</Value></ParameterValueStruct></ParameterList></cwmp:Inform>");
        let parsed = parse(&input).unwrap();
        assert_eq!(parsed.serial, "SN&1"); assert_eq!(parsed.id, "id<&"); assert_eq!(parsed.data["parameters"][0]["value"], "a<b");
        assert!(parse("<!DOCTYPE x [<!ENTITY a 'b'>]><x/>").is_err());
        let fault = parse(&envelope("urn:dslforum-org:cwmp-1-0", "x", "<soap:Fault><detail><cwmp:Fault><FaultCode>9005</FaultCode><FaultString>Unknown</FaultString></cwmp:Fault></detail></soap:Fault>")).unwrap();
        assert_eq!(fault.data["fault_code"], "9005");
    }
    #[test] fn typed_write_is_escaped() {
        let xml = request("urn:dslforum-org:cwmp-1-0", 1, "SetParameterValues", &serde_json::json!({"parameters":[{"name":"x", "value":"<ssid>&", "kind":"string"}]})).unwrap();
        assert!(xml.contains("&lt;ssid&gt;&amp;")); assert!(parse(&xml).is_ok());
        assert!(request("urn:dslforum-org:cwmp-1-0", 1, "SetParameterValues", &serde_json::json!({"parameters":[{"name":"x", "value":"a", "kind":"evil"}]})).is_err());
    }
}
