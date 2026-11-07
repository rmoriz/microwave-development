use std::process::ExitCode;

fn main() -> ExitCode {
    match microwave_ctl::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(microwave_ctl::CtlError::Exit(code)) => ExitCode::from(code),
        Err(microwave_ctl::CtlError::Coded { code, message }) => {
            eprintln!("error: {message}");
            ExitCode::from(code)
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
