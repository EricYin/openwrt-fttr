fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--capabilities"] {
        println!("{}", h3c_fttr_tools::capabilities::json());
        return;
    }
    if !args.is_empty() {
        eprintln!("Usage: fttrd [--capabilities]");
        std::process::exit(1);
    }
    if let Err(error) = h3c_fttr_tools::capabilities::publish_menu() {
        eprintln!("[-] Could not refresh LuCI capability markers: {error}");
    }
    h3c_fttr_tools::controller::run();
}
