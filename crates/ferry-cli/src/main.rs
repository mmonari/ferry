use std::process::ExitCode;

fn main() -> ExitCode {
    let code = ferry_cli::run(
        std::env::args_os(),
        &ferry_cli::client::socket_path(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}
