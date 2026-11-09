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
    if args == ["--platform-info"] {
        println!("{}", openwrt_fttr::platform::runtime::report_json());
        return;
    }
    if args == ["--platform-check"] {
        if openwrt_fttr::platform::runtime::detect().is_err() {
            std::process::exit(1);
        }
        return;
    }
    if !args.is_empty() {
        eprintln!("Usage: fttrd [--capabilities|--platform-info|--platform-check]");
        std::process::exit(1);
    }
    if let Err(error) = openwrt_fttr::platform::runtime::detect() {
        eprintln!("[-] Platform selection failed closed: {error}");
        std::process::exit(1);
    }
    if let Err(error) = openwrt_fttr::capabilities::publish_menu() {
        eprintln!("[-] Could not refresh LuCI capability markers: {error}");
    }
    if let Err(error) = openwrt_fttr::platform::runtime::run() {
        eprintln!("[-] Platform runtime failed: {error}");
        std::process::exit(1);
    }
}
