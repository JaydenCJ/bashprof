use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match bashprof::cli::parse(&args) {
        Ok(action) => ExitCode::from(bashprof::cli::dispatch(action).clamp(0, 255) as u8),
        Err(msg) => {
            eprintln!("bashprof: {msg}");
            ExitCode::from(2)
        }
    }
}
