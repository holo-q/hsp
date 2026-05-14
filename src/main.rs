use std::path::PathBuf;
use std::time::Duration;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        print_workgroup_probe(None);
        return Ok(());
    };

    match command {
        "broker" => hsp::serve_default().map_err(Into::into),
        "ping" => request("ping"),
        "status" => request("status"),
        "shutdown" => request_without_start("shutdown"),
        "socket" => {
            println!("{}", hsp::socket_path().display());
            Ok(())
        }
        "workgroup" => {
            print_workgroup_probe(args.get(1).map(PathBuf::from));
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        _ => {
            print_workgroup_probe(Some(PathBuf::from(command)));
            Ok(())
        }
    }
}

fn request(method: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = hsp::BrokerClient::from_default_path();
    client.connect_or_start(Duration::from_millis(250), Duration::from_secs(5))?;
    let result = client.request(method, serde_json::Map::new())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn request_without_start(method: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = hsp::BrokerClient::from_default_path();
    client.connect()?;
    let result = client.request(method, serde_json::Map::new())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn print_workgroup_probe(path: Option<PathBuf>) {
    let path =
        path.unwrap_or_else(|| std::env::current_dir().expect("current directory is available"));
    let workspace = hsp::HspWorkspace::discover(&path);

    println!("hsp {}", env!("CARGO_PKG_VERSION"));
    println!("root: {}", workspace.root.display());
    println!("python_reference: {}", workspace.py_reference.display());

    if workspace.workgroups.is_empty() {
        println!("workgroups: none");
        return;
    }

    println!("workgroups:");
    for workgroup in workspace.workgroups {
        println!(
            "  {} {} {}",
            workgroup.level.as_str(),
            workgroup.name,
            workgroup.root.display()
        );
    }
}

fn print_help() {
    println!("hsp {}", env!("CARGO_PKG_VERSION"));
    println!("usage:");
    println!("  hsp [path]");
    println!("  hsp workgroup [path]");
    println!("  hsp broker");
    println!("  hsp socket");
    println!("  hsp ping|status|shutdown");
}
