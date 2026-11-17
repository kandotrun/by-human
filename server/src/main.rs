use by_human::store::Store;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("check") => {
            let Ok(database_url) = std::env::var("DATABASE_URL") else {
                eprintln!("by-human: DATABASE_URL is required");
                return std::process::ExitCode::FAILURE;
            };
            match Store::connect(&database_url).await {
                Ok(_) => {
                    println!("by-human: PostgreSQL schema is ready");
                    std::process::ExitCode::SUCCESS
                }
                Err(_) => {
                    eprintln!("by-human: PostgreSQL could not be initialized");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        None | Some("--help" | "-h") => {
            println!(
                "Usage: by-human check\n\nCheck the configured PostgreSQL database and apply server migrations."
            );
            std::process::ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("by-human: unknown command; use --help");
            std::process::ExitCode::FAILURE
        }
    }
}
