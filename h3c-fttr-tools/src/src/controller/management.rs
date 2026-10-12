//! Standard G.988 ME 340 bootstrap. Association readback is not WAN/Inform proof.
use crate::{controller::business::{create_verified,verify},pon_engine::omci_transport::{OmciSession,is_unknown_instance}};
use std::io;
pub fn configure(session:&mut OmciSession,instance:u16,url:&str,vlan:u32,priority:u32)->io::Result<()> {
    let tag=management_tag(vlan,priority)?;
    if std::env::var("FTTR_ACS_USERNAME").is_ok_and(|v|!v.is_empty()) {return Err(io::Error::new(io::ErrorKind::Unsupported,"OMCI authentication association not verified"));}
    if !url.starts_with("http://") || url.len()>374 || !url.is_ascii() || url.bytes().any(|b|b.is_ascii_control()) {return Err(io::Error::new(io::ErrorKind::InvalidInput,"invalid OMCI ACS URL"));}
    let before=session.get(340,instance,0xe000)?;
    let original=before.payload[3..8].to_vec();
    if original!=[1,0xff,0xff,0xff,0xff] && !(original[0]<=1 && original[1..3]==[0x80,1]) {return Err(io::Error::new(io::ErrorKind::AlreadyExists,"foreign OMCI ACS association"));}
    match session.get(157,0x200,0x8000) {
        Ok(frame)=>{
            let count=frame.payload[3] as usize; if count>15 {return Err(io::Error::new(io::ErrorKind::InvalidData,"invalid OMCI URL length"));}
            let mut value=Vec::new(); for n in 0..count {let part=session.get(157,0x200,0x4000>>n)?;value.extend_from_slice(&part.payload[3..28]);}
            while value.last()==Some(&0) {value.pop();}
            if value!=url.as_bytes() {return Err(io::Error::new(io::ErrorKind::AlreadyExists,"existing OMCI ACS URL differs"));}
        },
        Err(e) if is_unknown_instance(&e)=>{
            session.create(157,0x200,&[])?;
            for (n,part) in url.as_bytes().chunks(25).enumerate() {let mut data=[0;25];data[..part.len()].copy_from_slice(part);session.set(157,0x200,0x4000>>n,&data)?;verify(session,157,0x200,0x4000>>n,&data)?;}
            let count=url.len().div_ceil(25) as u8;session.set(157,0x200,0x8000,&[count])?;verify(session,157,0x200,0x8000,&[count])?;
        },
        Err(e)=>return Err(e),
    }
    create_verified(session,137,0x8001,&[0,0,2,0],0xc000,&[0,0,2,0])?;
    let association=[0x80,1,tag[0],tag[1]];
    if original==[0,0x80,1,tag[0],tag[1]] {return Ok(());}
    let result=(||{session.set(340,instance,0x6000,&association)?;verify(session,340,instance,0x6000,&association)?;session.set(340,instance,0x8000,&[0])?;verify(session,340,instance,0xe000,&[0,0x80,1,tag[0],tag[1]])})();
    if result.is_err() {let _=session.set(340,instance,0xe000,&original);}
    result
}
fn management_tag(vlan:u32,priority:u32)->io::Result<[u8;2]> {
    if vlan>4094 || priority!=0 {return Err(io::Error::new(io::ErrorKind::InvalidInput,"management VLAN priority contract only verified at zero"));}
    Ok(if vlan==0 {0xffff}else{vlan as u16}.to_be_bytes())
}
#[cfg(test)] mod tests {
    #[test] fn standard_management_tag_rejects_reserved_vlan_and_unverified_priority() {
        assert_eq!(super::management_tag(0,0).unwrap(),[255,255]);assert_eq!(super::management_tag(46,0).unwrap(),[0,46]);
        assert!(super::management_tag(4095,0).is_err());assert!(super::management_tag(46,1).is_err());
    }
}
pub fn bootstrap(session:&mut OmciSession,onu:u8,instance:u16) {
    let serial=crate::ipc::send_ipc_command("STATUS").ok().and_then(|v|serde_json::from_str::<crate::model::FttrSystemStatus>(&v).ok()).and_then(|s|s.sub_gateways.into_iter().find(|d|d.onu_id==onu).map(|d|d.serial_number));
    let Some(serial)=serial else {return;};
    let Some((desired,revision))=crate::provisioning::management_desired(&serial) else {return;};
    let result=configure(session,instance,&desired.acs_url,desired.vlan,desired.priority);
    crate::provisioning::management_result(&serial,serde_json::json!({"revision":revision,"state":if result.is_ok(){"association_verified"}else{"blocked"},"reason":result.err().map(|e|e.to_string()),"vlan":desired.vlan,"updated_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}));
}
