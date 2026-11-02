//! Persisted offload settings shared by the CLI and native controller.
use std::{io, path::Path};

pub fn set(enabled: bool, bridge: Option<&str>) -> io::Result<()> {
    if let Some(bridge) = bridge {
        if bridge.is_empty()
            || bridge.len() > 15
            || !bridge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(io::Error::other("Invalid bridge interface"));
        }
    }
    let delta = Path::new("/tmp/.uci/fttr");
    if delta.exists() && std::fs::metadata(delta)?.len() > 0 {
        return Err(io::Error::other(
            "FTTR has pending UCI changes; apply or revert them first",
        ));
    }
    let mut uci = openwrt_uci::Uci::new().map_err(io::Error::other)?;
    uci.ensure_section("fttr", "offload", "offload")
        .map_err(io::Error::other)?;
    uci.set(
        "fttr",
        "offload",
        "enabled",
        if enabled { "1" } else { "0" },
    )
    .map_err(io::Error::other)?;
    if let Some(bridge) = bridge {
        uci.set("fttr", "offload", "bridge", bridge)
            .map_err(io::Error::other)?;
    }
    uci.commit("fttr").map_err(io::Error::other)?;
    if !enabled {
        let mask = "/sys/module/h3c_fmcs/parameters/bridge_offload_mask";
        std::fs::write(mask, "0\n")?;
        if std::fs::read_to_string(mask)?.trim() != "0" {
            return Err(io::Error::other(
                "Saved, but driver withdrawal is not confirmed",
            ));
        }
    }
    Ok(())
}
