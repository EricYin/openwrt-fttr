//! Reconcile only WAN objects allocated by this controller. Never adopt or prune
//! an arbitrary vendor WAN based merely on its ordinal instance number.
use super::{acs, config::Device};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const ROOT: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.";
fn registry_path(id: &str) -> String {
    let hash=id.bytes().fold(0xcbf29ce484222325u64,|h,b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("/etc/fttr/wan-owned/{hash:016x}.json")
}
pub fn load(id: &str) -> Result<Option<BTreeMap<String,Owned>>,String> {
    let path=registry_path(id);
    let text=match std::fs::read_to_string(path) { Ok(v)=>v,Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),Err(e)=>return Err(format!("read WAN ownership: {e}")) };
    let v: Value=serde_json::from_str(&text).map_err(|e|format!("invalid WAN ownership: {e}"))?;
    if v["cwmp_id"] != id { return Err("WAN ownership identity mismatch".into()); }
    serde_json::from_value(v["owned"].clone()).map(Some).map_err(|e|format!("invalid WAN ownership records: {e}"))
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Owned {
    pub parent: u32, pub connection: u32, pub state: String, pub service_list: String,
}
#[derive(Clone)]
pub struct Reconciler {
    pub owned: BTreeMap<String, Owned>, pub results: BTreeMap<String, Value>,
    actions: VecDeque<(String,bool)>,
    pub stage: String, task: Option<u64>, current: Option<(String,bool)>, initialized: bool,
}
impl Reconciler {
    pub fn new(device: &Device, owned: BTreeMap<String,Owned>, prune: bool) -> Self {
        let mut actions=VecDeque::new();
        if prune { for id in owned.keys() { if !device.items.iter().any(|i| i.wan_ref.as_ref() == Some(id)) { actions.push_back((id.clone(),true)); } } }
        for i in &device.items { if i.kind == "device_service" && i.auto_instance.is_some() { actions.push_back((i.id.clone(),false)); } }
        Self{owned,results:BTreeMap::new(),actions,stage:"waiting".into(),task:None,current:None,initialized:false}
    }
    fn queue(&mut self, device: &Device, method: &str, args: Value, stage: &str) {
        if self.save(&device.cwmp_id).is_err() { self.fail(device,"blocked","cannot persist WAN ownership before lifecycle RPC"); return; }
        let v=acs(json!({"op":"queue","device":device.cwmp_id,"method":method,"args":args}));
        if let Some(id)=v["id"].as_u64() { self.task=Some(id); self.stage=stage.into(); }
        else { self.fail(device,"failed","cannot queue WAN lifecycle RPC"); }
    }
    fn fail(&mut self, device: &Device, state: &str, reason: &str) {
        let id=self.current.as_ref().map(|a| a.0.as_str()).unwrap_or("inventory");
        self.results.insert(id.into(),json!({"state":state,"reason":reason,"task_id":self.task,"cwmp_id":device.cwmp_id}));
        self.task=None; self.current=None; self.stage="next".into();
    }
    pub fn tick(&mut self, device: &mut Device, apply: bool, _management: bool, reachable: bool) -> bool {
        if self.stage == "done" { return true; }
        if self.actions.is_empty() && self.current.is_none() { self.stage="done".into(); return true; }
        if !reachable { return false; }
        if self.task.is_none() && acs(json!({"op":"busy","device":device.cwmp_id}))["busy"] == true { return false; }
        if !self.initialized {
            self.initialized=true;
            self.queue(device,"GetParameterNames",json!({"path":ROOT,"next_level":true}),"inventory"); return false;
        }
        if let Some(task)=self.task {
            let v=acs(json!({"op":"task","id":task})); let state=v["state"].as_str().unwrap_or("missing");
            if matches!(state,"queued" | "sent") { return false; }
            if !matches!(state,"completed" | "accepted_pending_apply") {
                if matches!(state,"fault" | "expired") && self.stage == "adding_parent" {
                    if let Some((id,_))=&self.current { self.owned.remove(id); }
                }
                self.fail(device,if state == "unknown" {"unknown"} else {"failed"},"WAN lifecycle RPC failed/unknown; no write replay");
                if self.stage == "next" && self.current.is_none() && !self.results.contains_key("inventory") { return false; }
                self.actions.clear(); self.stage="done".into(); return true;
            }
            self.task=None;
            match self.stage.as_str() {
                "inventory" => {
                    let names: BTreeSet<String>=v["result"]["parameters"].as_array().into_iter().flatten().filter_map(|p| p["name"].as_str().map(str::to_string)).collect();
                    self.owned.retain(|_,o| o.parent == 0 || o.state != "ready" || names.contains(&format!("{ROOT}{}.",o.parent)));
                    self.stage="next".into();
                },
                "adding_parent" => {
                    let n=v["result"]["instance_number"].as_str().and_then(|n| n.parse::<u32>().ok()).filter(|n| *n>0);
                    let Some(n)=n else { self.fail(device,"unknown","AddObject did not return an instance"); return false; };
                    let id=self.current.as_ref().unwrap().0.clone(); let o=self.owned.get_mut(&id).unwrap(); o.parent=n; o.state="discovering_child".into();
                    self.queue(device,"GetParameterNames",json!({"path":format!("{ROOT}{n}."),"next_level":false}),"child_names"); return false;
                },
                "child_names" => {
                    let id=self.current.as_ref().unwrap().0.clone(); let o=&self.owned[&id];
                    let prefix=format!("{ROOT}{}.WANIPConnection.",o.parent);
                    let children: Vec<u32>=v["result"]["parameters"].as_array().into_iter().flatten().filter_map(|p| p["name"].as_str()?.strip_prefix(&prefix)?.strip_suffix('.')?.parse::<u32>().ok()).collect();
                    if children.len() > 1 { self.fail(device,"blocked","new WAN unexpectedly has multiple IP connections"); return false; }
                    if let Some(n)=children.first() { let o=self.owned.get_mut(&id).unwrap(); o.connection=*n; o.state="ready".into(); self.remap(device,&id); self.current=None; self.stage="next".into(); }
                    else { self.owned.get_mut(&id).unwrap().state="adding_child".into(); self.queue(device,"AddObject",json!({"object":prefix}),"adding_child"); return false; }
                },
                "adding_child" => {
                    let n=v["result"]["instance_number"].as_str().and_then(|n| n.parse::<u32>().ok()).filter(|n| *n>0);
                    let Some(n)=n else { self.fail(device,"unknown","child AddObject did not return an instance"); return false; };
                    let id=self.current.as_ref().unwrap().0.clone(); let o=self.owned.get_mut(&id).unwrap(); o.connection=n; o.state="ready".into();
                    self.remap(device,&id); self.current=None; self.stage="next".into();
                },
                "delete_guard" => {
                    let id=self.current.as_ref().unwrap().0.clone(); let o=&self.owned[&id];
                    let observed=v["result"]["parameters"].as_array().and_then(|a| a.first()).and_then(|p| p["value"].as_str());
                    let Some(observed)=observed else { self.fail(device,"blocked","missing service list before deleting owned WAN"); return false; };
                    if observed.contains("TR069") || (!observed.is_empty() && observed != o.service_list) {
                        self.fail(device,"blocked","owned WAN now carries protected or reassigned service; manual review required"); return false;
                    }
                    if !apply { self.fail(device,"drift","owned WAN removed from desired configuration; apply will delete"); return false; }
                    let object=format!("{ROOT}{}.",o.parent); self.owned.get_mut(&id).unwrap().state="deleting".into();
                    self.queue(device,"DeleteObject",json!({"object":object}),"deleting"); return false;
                },
                "deleting" => {
                    self.queue(device,"GetParameterNames",json!({"path":ROOT,"next_level":true}),"verify_delete"); return false;
                },
                "verify_delete" => {
                    let id=self.current.as_ref().unwrap().0.clone(); let path=format!("{ROOT}{}.",self.owned[&id].parent);
                    let exists=v["result"]["parameters"].as_array().is_some_and(|a| a.iter().any(|p| p["name"] == path));
                    if exists { self.fail(device,"unknown","DeleteObject accepted but WAN still exists; no replay"); return false; }
                    self.owned.remove(&id); self.results.insert(id,json!({"state":"deleted","reason":"owned WAN removed and absence verified","task_id":task})); self.current=None; self.stage="next".into();
                },
                _ => (),
            }
        }
        if self.current.is_none() {
            let Some((id,delete))=self.actions.pop_front() else { self.stage="done".into(); return true; };
            self.current=Some((id.clone(),delete));
            if delete {
                let Some(o)=self.owned.get(&id) else { self.current=None; return false; };
                if o.state != "ready" { self.fail(device,"unknown","owned WAN lifecycle interrupted; automatic deletion withheld"); return false; }
                let guard=format!("{ROOT}{}.WANIPConnection.{}.{}",o.parent,o.connection,if device.adapter == "hl202" {"X_CU_ServiceList"} else {"X_HW_SERVICELIST"});
                self.queue(device,"GetParameterValues",json!({"names":[guard]}),"delete_guard"); return false;
            }
            if let Some(o)=self.owned.get(&id) {
                if o.state == "ready" { self.remap(device,&id); self.current=None; return false; }
                self.fail(device,"unknown","WAN creation interrupted; automatic AddObject replay withheld"); return false;
            }
            let item=device.items.iter().find(|i| i.id == id).unwrap();
            if item.service_type == "tr069" { self.fail(device,"blocked","TR069 is owned by OMCI"); return false; }
            if !apply { self.fail(device,"drift","WAN missing; apply will allocate parent and IP connection"); return false; }
            let service_list=item.parameters.iter().find(|p| p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST")).map(|p| p.value.clone()).unwrap_or_default();
            self.owned.insert(id,Owned{parent:0,connection:0,state:"adding_parent".into(),service_list});
            self.queue(device,"AddObject",json!({"object":ROOT}),"adding_parent");
        }
        false
    }
    fn remap(&self, device: &mut Device, id: &str) {
        let o=&self.owned[id];
        for item in &mut device.items { if item.wan_ref.as_deref() == Some(id) { if let Some(old)=item.auto_instance {
            let old=format!("{ROOT}{old}."); let new=format!("{ROOT}{}.",o.parent);
            for p in &mut item.parameters { p.name=p.name.replace(&old,&new).replace("WANIPConnection.1.",&format!("WANIPConnection.{}.",o.connection)); }
            if let Some(g)=&mut item.guard { *g=g.replace(&old,&new).replace("WANIPConnection.1.",&format!("WANIPConnection.{}.",o.connection)); }
        } } }
    }
    pub fn block_unresolved(&self, device: &mut Device) {
        for item in &mut device.items { if let Some(id)=&item.wan_ref { if item.auto_instance.is_some() && self.results.contains_key(id) {
            item.blocked=Some(self.results[id]["reason"].as_str().unwrap_or("WAN reconciliation failed").into());
        } } }
    }
    pub fn save(&self, id: &str) -> Result<(),String> {
        #[cfg(test)] { let _=id; return Ok(()); }
        #[cfg(not(test))] {
            use std::{io::Write,os::unix::fs::OpenOptionsExt};
            let text=json!({"cwmp_id":id,"owned":self.owned}).to_string(); let path=registry_path(id);
            if std::fs::read_to_string(&path).ok().as_deref() == Some(&text) { return Ok(()); }
            let result=(|| -> std::io::Result<()> {
                std::fs::create_dir_all("/etc/fttr/wan-owned")?;
                let tmp=format!("{path}.tmp"); let mut f=std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
                f.write_all(text.as_bytes())?; f.sync_all()?; std::fs::rename(tmp,path)?;
                std::fs::File::open("/etc/fttr/wan-owned")?.sync_all()
            })(); result.map_err(|e|format!("persist WAN ownership: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{RPC,config::{Item,Parameter}};
    fn device() -> Device { Device{id:"child".into(),optical_sn:"HWTC12345678".into(),cwmp_id:"id".into(),adapter:"hwtc".into(),items:vec![Item{id:"internet".into(),kind:"device_service".into(),service_type:"internet".into(),guard:Some(format!("{ROOT}1000.WANIPConnection.1.X_HW_SERVICELIST")),blocked:None,wan_ref:Some("internet".into()),auto_instance:Some(1000),parameters:vec![Parameter{name:format!("{ROOT}1000.WANIPConnection.1.X_HW_SERVICELIST"),value:"INTERNET".into(),kind:"string".into(),secret:false}]}],wifi_cleanup:"preserve".into(),wifi_preserve:BTreeSet::new()} }
    fn responses(v: Vec<Value>) { RPC.with(|r| *r.borrow_mut()=(v.into(),Vec::new())); }
    fn ok(parameters: Value) -> Value { json!({"state":"completed","result":{"parameters":parameters}}) }
    #[test] fn auto_creation_uses_returned_parent_and_child_instances() {
        let mut d=device(); let mut r=Reconciler::new(&d,BTreeMap::new(),true);
        responses(vec![json!({"busy":false}),json!({"id":1}),ok(json!([])),json!({"id":2}),json!({"state":"completed","result":{"instance_number":"7"}}),json!({"id":3}),ok(json!([])),json!({"id":4}),json!({"state":"completed","result":{"instance_number":"3"}})]);
        for _ in 0..5 { r.tick(&mut d,true,false,true); }
        assert_eq!(r.owned["internet"].parent,7); assert_eq!(r.owned["internet"].connection,3);
        assert_eq!(d.items[0].parameters[0].name,format!("{ROOT}7.WANIPConnection.3.X_HW_SERVICELIST"));
    }
    #[test] fn plan_allocates_nothing_and_unknown_add_is_not_replayed() {
        let mut d=device(); let mut r=Reconciler::new(&d,BTreeMap::new(),true);
        responses(vec![json!({"busy":false}),json!({"id":1}),ok(json!([]))]);
        r.tick(&mut d,false,false,true); r.tick(&mut d,false,false,true);
        assert_eq!(r.results["internet"]["state"],"drift"); assert!(r.owned.is_empty());
        let mut r=Reconciler::new(&d,BTreeMap::new(),true);
        responses(vec![json!({"busy":false}),json!({"id":1}),ok(json!([])),json!({"id":2}),json!({"state":"unknown"})]);
        for _ in 0..3 { r.tick(&mut d,true,false,true); }
        assert_eq!(r.results["internet"]["state"],"unknown"); assert_eq!(r.owned["internet"].parent,0);
    }
    #[test] fn pruning_refuses_a_management_or_reassigned_wan() {
        let mut d=device(); d.items.clear(); let mut owned=BTreeMap::new(); owned.insert("old".into(),Owned{parent:2,connection:1,state:"ready".into(),service_list:"INTERNET".into()});
        let mut r=Reconciler::new(&d,owned,true);
        responses(vec![json!({"busy":false}),json!({"id":1}),ok(json!([{"name":format!("{ROOT}2.")}])),json!({"id":2}),ok(json!([{"value":"TR069_INTERNET"}]))]);
        for _ in 0..3 { r.tick(&mut d,true,false,true); }
        assert_eq!(r.results["old"]["state"],"blocked"); assert!(r.owned.contains_key("old"));
    }
}
