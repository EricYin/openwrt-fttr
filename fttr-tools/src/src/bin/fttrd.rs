// Only the daemon selects this allocator; CLI and standalone tools keep System.
#[cfg(feature = "allocator-mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--capabilities"] {
        println!("{}", openwrt_fttr::capabilities::json());
        return;
    }
    if !args.is_empty() {
        eprintln!("Usage: fttrd [--capabilities]");
        std::process::exit(1);
    }
    if let Err(error) = openwrt_fttr::capabilities::publish_menu() {
        eprintln!("[-] Could not refresh LuCI capability markers: {error}");
    }
    openwrt_fttr::controller::run();
}
