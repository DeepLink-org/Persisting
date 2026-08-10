fn main() -> anyhow::Result<()> {
    if persisting_pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    match persisting_pvisor::sandbox::run_internal_if_requested() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            eprintln!("pVisor local sandbox setup failed: {error:#}");
            std::process::exit(persisting_pvisor::sandbox::SANDBOX_SETUP_EXIT_CODE);
        }
    }
    persisting_pvisor::cli::main()
}
