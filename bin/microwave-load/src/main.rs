use std::process::ExitCode;

fn main() -> ExitCode {
    match microwave_load::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
