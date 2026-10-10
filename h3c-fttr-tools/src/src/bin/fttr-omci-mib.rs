fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let id = args.get(1).ok_or_else(|| std::io::Error::other("usage: fttr-omci-mib ONU_ID [--summary]"))?
        .parse().map_err(std::io::Error::other)?;
    h3c_fttr_tools::controller::inventory::read(id, args.iter().any(|arg| arg == "--summary"))
}
