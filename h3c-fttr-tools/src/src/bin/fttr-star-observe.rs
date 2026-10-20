use h3c_fttr_tools::pon_engine::omci::platform::fmcs::PacketSocket;
use std::{collections::BTreeMap, io, time::{Duration, Instant}};

fn main() -> io::Result<()> {
    let socket = PacketSocket::open("fttr0")?;
    let start = Instant::now();
    let mut buffer = [0u8; 4096];
    let mut counts = BTreeMap::new();
    let mut star = 0u64;
    println!("PASSIVE duration=90s expected_source=ec:cf:70:ec:54:88 no_transmit=true");
    while start.elapsed() < Duration::from_secs(90) {
        let Some((bytes, kind, aux)) = socket.receive_into(&mut buffer)? else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if kind == 4 || bytes.len() < 14 {
            continue;
        }
        let gem = aux.map(|tag| tag & 0xfff);
        *counts.entry(gem).or_insert(0u64) += 1;
        if bytes[6..12] != [0xec, 0xcf, 0x70, 0xec, 0x54, 0x88] {
            continue;
        }
        star += 1;
        let mut offset = 12;
        let mut tags = Vec::new();
        while bytes.len() >= offset + 6 && matches!(&bytes[offset..offset + 2], [0x81, 0] | [0x88, 0xa8]) {
            tags.push(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
            offset += 4;
        }
        let ip = offset + 2;
        let dhcp = bytes.len() >= ip + 28 && bytes[offset..offset + 2] == [8, 0]
            && bytes[ip + 9] == 17 && {
                let udp = ip + usize::from(bytes[ip] & 15) * 4;
                bytes.len() >= udp + 4 && bytes[udp..udp + 4] == [0, 68, 0, 67]
            };
        if star <= 20 {
            let limit = if dhcp { bytes.len() } else { bytes.len().min(ip + 28) };
            let hex: String = bytes[..limit].iter().map(|byte| format!("{byte:02x}")).collect();
            println!("STAR elapsed_ms={} gem={gem:?} aux={aux:?} tags={tags:?} len={} dhcp={dhcp} hex={hex}", start.elapsed().as_millis(), bytes.len());
        }
    }
    println!("SUMMARY star_source_frames={star} received_envelopes={counts:?}");
    Ok(())
}
