mod runtime;
mod scenarios;
mod trace;

use runtime::{Budget, Server, tool_version};
use serde_json::json;
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use trace::{Action, Config, Plan};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}

struct Options {
    binary: PathBuf,
    output: PathBuf,
    plan: Plan,
    seconds: u64,
}
impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut binary = None;
        let mut output = PathBuf::from("fux-fuzz/runs");
        let mut scenario = "all".to_owned();
        let mut seed = 1;
        let mut iterations = 1;
        let mut count = 6;
        let mut seconds = 120;
        let mut replay = None;
        let mut generated_options = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            if matches!(arg.as_str(), "--help" | "-h") {
                println!(
                    "fux-fuzz --fux PATH [--scenario all|startup|resize|shutdown|paste|signal|keys|mouse|copy|history|zoom|layout|process|nav|scene|config|overlay|limits|chrome|selection] [--seed N]\n  [--iterations 1..100] [--actions 1..200] [--seconds 1..600] [--output DIR]\nfux-fuzz --fux PATH --replay TRACE.json [--seconds N] [--output DIR]\nNo implicit build. Default smoke: all scenarios, seed 1, one iteration, six generated resizes.\nStress is opt-in via --iterations/--actions. Failures exit nonzero and retain bundles."
                );
                return Ok(None);
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?;
            match arg.as_str() {
                "--fux" => binary = Some(PathBuf::from(value)),
                "--output" => output = value.into(),
                "--scenario" => {
                    scenario = value;
                    generated_options = true;
                }
                "--seed" => {
                    seed = value.parse()?;
                    generated_options = true;
                }
                "--iterations" => {
                    iterations = value.parse()?;
                    generated_options = true;
                }
                "--actions" => {
                    count = value.parse()?;
                    generated_options = true;
                }
                "--seconds" => seconds = value.parse()?,
                "--replay" => replay = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown option: {arg}").into()),
            }
        }
        ensure((1..=600).contains(&seconds), "seconds must be 1..600")?;
        ensure(
            replay.is_none() || !generated_options,
            "replay cannot be combined with generation options",
        )?;
        let plan = match replay {
            Some(path) => Plan::read(&path)?,
            None => Plan::generate(&scenario, seed, iterations, count)?,
        };
        plan.validate()?;
        let binary = binary
            .ok_or("--fux must explicitly identify an already-built binary")?
            .canonicalize()?;
        ensure(binary.is_file(), "fux binary is not a file")?;
        fs::create_dir_all(&output)?;
        Ok(Some(Self {
            binary,
            output: output.canonicalize()?,
            plan,
            seconds,
        }))
    }
}

fn binary_identity(path: &Path, budget: &Budget) -> Result<serde_json::Value> {
    // Streaming, constant memory. This is an identity hint, not a security hash.
    let mut file = fs::File::open(path)?;
    let mut hash = 0xcbf29ce484222325u64;
    let mut chunk = [0; 65536];
    loop {
        budget.check(budget.end)?;
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        for byte in chunk.get(..n).ok_or("invalid identity read")? {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
    }
    Ok(json!({"path":path,"size":file.metadata()?.len(),"fnv1a64":format!("{hash:016x}")}))
}
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
fn run(options: Options) -> Result<()> {
    let started = Instant::now();
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, interrupted.clone())?;
    }
    let budget = Arc::new(Budget {
        end: started + Duration::from_secs(options.seconds),
        interrupted: interrupted.clone(),
    });
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = options
        .output
        .join(format!("run-{stamp}-{}", std::process::id()));
    fs::create_dir(&directory)?;
    let trace_path = directory.join("trace.json");
    fs::write(&trace_path, serde_json::to_vec_pretty(&options.plan)?)?;
    let replay = format!(
        "{} --fux {} --replay {} --seconds {} --output {}",
        quoted(&env::current_exe()?),
        quoted(&options.binary),
        quoted(&trace_path),
        options.seconds,
        quoted(&options.output)
    );
    fs::write(directory.join("replay.txt"), format!("{replay}\n"))?;
    fs::write(
        directory.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "binary":binary_identity(&options.binary, &budget)?,"harness_version":env!("CARGO_PKG_VERSION"),
            "os":env::consts::OS,"arch":env::consts::ARCH,"uname":tool_version("uname", "-a", &budget)?,
            "rustc":tool_version("rustc", "-Vv", &budget)?,"cargo":tool_version("cargo", "-V", &budget)?,
            "seed":options.plan.seed,"deadline_seconds":options.seconds,"operation_seconds":5,
            "capture_tail_bytes":65536,"response_limit_bytes":1048576,"event_limit_bytes_per_case":4194304,
            "environment":"children use env_clear; only PATH, HOME, SHELL, TERM, PS1, HISTFILE and frontend FUX_ENDPOINT are set"
        }))?,
    )?;
    println!("Bundle: {}", directory.display());
    let mut results = Vec::new();
    let mut failures = 0;
    for (index, action) in options.plan.actions.iter().enumerate() {
        if let Err(e) = budget.check(budget.end) {
            failures += 1;
            results.push(json!({"case":index,"failure":e.to_string(),"remaining":"not executed"}));
            break;
        }
        let case_started = Instant::now();
        let case_dir = directory.join(format!("case-{index:03}"));
        let config = match action {
            Action::Startup { config } => *config,
            Action::Copy { reload: false } | Action::History => Config::Clipboard,
            _ => Config::Missing,
        };
        let outcome = match Server::spawn(&options.binary, &case_dir, config, budget.clone()) {
            Ok(mut server) => {
                let execution = server
                    .journal
                    .record("begin_scenario", serde_json::to_value(action)?)
                    .and_then(|_| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            scenarios::execute(&mut server, action)
                        }))
                        .map_err(|payload| {
                            let message = payload
                                .downcast_ref::<String>()
                                .map(String::as_str)
                                .or_else(|| payload.downcast_ref::<&str>().copied())
                                .unwrap_or("non-string panic");
                            format!("harness/dependency panic: {message}")
                        })?
                    });
                let message = execution.as_ref().err().map(ToString::to_string);
                let diagnostic = server
                    .journal
                    .record("scenario_result", json!({"failure":message}));
                // Always attempt cleanup even if recording or the oracle failed.
                let cleanup = server.cleanup();
                match (execution, diagnostic, cleanup) {
                    (Ok(()), Ok(()), Ok(())) => Ok(()),
                    (a, b, c) => Err(format!(
                        "scenario: {}; diagnostics: {}; cleanup: {}",
                        a.err().map_or("ok".into(), |e| e.to_string()),
                        b.err().map_or("ok".into(), |e| e.to_string()),
                        c.err().map_or("ok".into(), |e| e.to_string())
                    )),
                }
            }
            Err(e) => Err(format!("setup: {e}")),
        };
        let elapsed = case_started.elapsed().as_millis();
        let failure = outcome.err();
        if let Some(error) = &failure {
            failures += 1;
            eprintln!("case {index} FAIL ({elapsed} ms): {error}");
        } else {
            println!("case {index} PASS ({elapsed} ms)");
            fs::remove_dir_all(&case_dir)?;
        }
        results.push(json!({"case":index,"action":action,"ms":elapsed,"failure":failure}));
        // Persist after each case so interruption still leaves completed results.
        fs::write(
            directory.join("summary.json"),
            serde_json::to_vec_pretty(
                &json!({"results":results,"failures":failures,"ms":started.elapsed().as_millis()}),
            )?,
        )?;
        if interrupted.load(Ordering::Relaxed) {
            break;
        }
    }
    fs::write(
        directory.join("summary.json"),
        serde_json::to_vec_pretty(
            &json!({"results":results,"failures":failures,"ms":started.elapsed().as_millis()}),
        )?,
    )?;
    println!(
        "{} cases, {failures} failures, {:.2}s\nReplay: {replay}",
        results.len(),
        started.elapsed().as_secs_f64()
    );
    ensure(failures == 0, "scenario run failed; see retained bundle")
}
fn main() -> ExitCode {
    match Options::parse().and_then(|o| o.map_or(Ok(()), run)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fux-fuzz: {error}");
            ExitCode::FAILURE
        }
    }
}
