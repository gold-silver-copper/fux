//! Repository development tooling, never linked into the fux runtime.
mod dependencies;
mod evidence;
mod freshness_capture;
mod gate_process;
mod gate_record;
mod headless_evidence;
mod headless_journal;
mod headless_performance;
mod inventory;
mod measure;
mod measure_frames;
mod measure_koh;
mod measure_memory;
mod measure_viewer;
mod package;
mod prompt_capture;
mod resource_capture;
mod resource_sampler;
mod retry_capture;
mod service_failure_capture;
mod setup_capture;
mod traffic_capture;
mod workflow_capture;

fn main() -> std::process::ExitCode {
    let result = (|| -> anyhow::Result<()> {
        let mut args = std::env::args().skip(1);
        match args.next().as_deref() {
            Some("dependencies") => dependencies::run(args.collect()),
            Some("scenario") => fux_xtask::scenarios::run(args.collect()),
            Some("fixture-worker") => fux_xtask::scenarios::worker(args.collect()),
            Some("detection-inventory") => inventory::run(args.collect()),
            Some("capture-input-retry") => retry_capture::run(args.collect()),
            Some("capture-prompt-boundary") => prompt_capture::run(args.collect()),
            Some("capture-service-failure") => service_failure_capture::run(args.collect()),
            Some("capture-resources") => resource_capture::run(args.collect()),
            Some("capture-traffic") => traffic_capture::run(args.collect()),
            Some("verify-capture-traffic") => evidence::verify_traffic(args.collect()),
            Some("capture-controller-setup") => setup_capture::run(args.collect()),
            Some("verify-controller-setup") => evidence::verify_setup(args.collect()),
            Some("verify-resources") => evidence::resource_regressions(),
            Some("capture-detection-freshness") => freshness_capture::run(args.collect()),
            Some("verify-detection-freshness") => evidence::verify_freshness(args.collect()),
            Some("capture-detection-screens") => evidence::capture_screens(args.collect()),
            Some("verify-detection-screens") => evidence::verify_screens(args.collect()),
            Some("headless-performance") => headless_performance::run(args.collect()),
            Some("headless-journal") => headless_journal::run(args.collect()),
            Some("verify-headless-performance") => headless_evidence::verify(args.collect()),
            Some("capture-workflow") => workflow_capture::run(args.collect()),
            Some("workflow-capture-worker") => workflow_capture::worker(),
            Some("verify-workflow") => evidence::verify_workflow(args.collect()),
            Some("resource-sampler-check") => resource_sampler::run(args.collect()),
            Some("resource-sampler-worker") => resource_sampler::worker(),
            Some("measure") => measure::run(args.collect()),
            Some("measure-frames") => measure_frames::run(args.collect()),
            Some("measure-memory") => measure_memory::run(args.collect()),
            Some("measure-viewer") => measure_viewer::run(args.collect()),
            Some("measure-koh") => measure_koh::run(args.collect()),
            Some("package-version") => package::run(args.collect()),
            _ => anyhow::bail!(
                "usage: fux-xtask dependencies <export|apply|verify> [--build] [--headless]\n       fux-xtask scenario NAME FUX_BINARY\n       fux-xtask fixture-worker NAME [ARGS]\n       fux-xtask package-version < cargo-metadata.json"
            ),
        }
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fux-xtask: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
