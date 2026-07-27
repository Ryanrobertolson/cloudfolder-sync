fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.iter().any(|argument| argument == "--service") {
        let database_path = arguments
            .windows(2)
            .find(|pair| pair[0] == "--database")
            .map(|pair| std::path::PathBuf::from(&pair[1]));
        match database_path {
            Some(path) => {
                if let Err(error) = cloudfolder_sync_lib::run_service(path) {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
            None => {
                eprintln!("CloudFolder service needs a database path");
                std::process::exit(2);
            }
        }
    } else {
        cloudfolder_sync_lib::run();
    }
}
