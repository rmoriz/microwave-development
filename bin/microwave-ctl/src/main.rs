use std::process::ExitCode;

fn main() -> ExitCode {
    match microwave_ctl::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
