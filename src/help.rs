//! fux's help, made from its words (`crate::script::Word`) and what fuxth
//! read of the libraries' words (`crate::script::docs`), so that it
//! always says what this binary does; and the guide to driving programs
//! from a script or an agent (`str agents explain`), which is
//! `docs/agents.md`, built in.
use crate::script::{Act, After, Word};

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");

/// Of the guide, `agents` its first screen (all before its first topic),
/// `guide` all of it, else the topic's section, headed `## TOPIC:`; `None`
/// if it has none so named.
fn guide(topic: &str) -> Option<String> {
    let first = AGENTS
        .split_once("\n## ")
        .map_or(AGENTS, |(first, _)| first);
    match topic {
        "agents" => return Some(first.to_owned()),
        "guide" => return Some(AGENTS.to_owned()),
        _ => {}
    }
    let heading = format!("\n## {topic}:");
    let (_, rest) = AGENTS.split_once(&heading)?;
    let section = rest
        .split_once("\n## ")
        .map_or(rest, |(section, _)| section);
    Some(format!("##{}{section}", heading.trim_start_matches("\n##")))
}

/// What `explain` says of a name that is neither a word nor a topic: the
/// topics, as the guide's headings name them.
pub fn unknown(name: &str) -> String {
    let topics = topics().join(" ");
    format!("no word or topic {name:?}; the topics: agents {topics} guide limits")
}

/// The guide's topics, as its headings name them.
fn topics() -> Vec<&'static str> {
    (AGENTS.split("\n## ").skip(1))
        .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
        .collect()
}

/// What `json` gives, word by word (`str json explain`).
pub const JSON: &str = include_str!("../docs/json.md");

/// How fux runs, and every word under its heading.
pub fn overview() -> String {
    let shebang = crate::script::SHEBANG;
    let from_opts = crate::script::from_opts("and");
    let mut out = format!(
        "\
usage: fux                     attach, starting a server if none runs
       fux WORD...             run the words, a fuxth program (or all in one argument)
       fux --file FILE ARG...  run a file of fuxth, its ARGs for `args` ({shebang})
       fux -                   run fuxth read from stdin

Words run left to right, each taking what the words before it left: a
number is a value; %2, @2 and +1 are a pane, a tab and a workspace; `str
WORD` pushes WORD as text. What is left is printed. What a word that acts
takes (options, keys, what to read) goes on the pane before it (`%2 str y
text send-keys`, `%2 all-history capture`); only
{from_opts} start from opts (`opts str jobs in str make
program str test arg run`). A new pane is laid out on a size given
(`opts 80 24 size …`), else the caller's own screen or tab (`fux` run in
a pane): run from outside fux, a size must be given, as fux makes none
up. An action answers at once,
with its pane's version from before it acted, one word
(`%2:0.57.0.3.0:RUN`); `str V changed wait` waits in V's pane until its
version differs from V, else answers `timeout` at this client's wait-seconds
limit unless `SECS timeout` says (see client-defaults.fux). A failure prints
`fux: CODE: MESSAGE` on stderr.
Driving programs from a script or an agent: `fux help agents`; one word:
`fux help WORD`.
",
    );
    // The language's parsing words and core, as fuxth says them: a block's
    // words once, never broken across lines.
    let mut shapes: Vec<String> = Vec::new();
    for (shape, _) in fuxth::Parsing::ALL.map(fuxth::Parsing::syntax) {
        let unbroken = shape.replace(' ', "\u{a0}");
        if !shapes.contains(&unbroken) {
            shapes.push(unbroken);
        }
    }
    let core = fuxth::Builtin::ALL.map(fuxth::Builtin::name).join(" ");
    let language = format!(
        "The language: {}; the core's words {core}; and the libraries below, written in \
         fuxth. Mistakes every run would make are refused before anything runs.",
        shapes.join(", ")
    );
    let words = language.split(' ').map(|w| w.replace('\u{a0}', " "));
    out.push_str(&wrapped(words, "\n", 74));
    out.push_str(
        "

Effects read `inputs -- outputs`; `a>b` maps input kind a to output kind b.
Alternative kinds retain their distinctions; requests must reach an action.
Below, x is a pane, tab, workspace or client (an action's answer is its
pane), a request on one, or opts; target, subject and place a pane (or a
tab, workspace or client) or a request on one; handle a pane, tab or
workspace; scope a handle or all; ref a name or a handle; text a string,
number or boolean. pane-builder is a pane or pane request; capture-source
maps a pane, answer, closed-answer or pane request to capture, and a
client to client-capture; sized is a
handle or headless client. replayable is a pane, tab or workspace, a
request on one, or all without a cursor; from on all leaves an
events-request, which only events takes.
wait and send-keys leave an answer for a pane, and answers for a tab or
workspace (send-keys: or a client), which no word that wants a pane takes.
",
    );
    let mut rows: Vec<_> = Word::ALL.iter().map(|w| w.row()).collect();
    rows.sort_by_key(|row| row.group);
    let mut group = None;
    for word in rows {
        if group != Some(word.group) {
            group = Some(word.group);
            out.push_str(&format!("\n{}:\n", word.group.name()));
        }
        out.push_str(&format!(
            "  {:<20} {:<28} {}\n",
            word.name, word.effect, word.about
        ));
    }
    out.push_str("\nThe libraries (`fux help WORD` says each):");
    let docs = crate::script::docs();
    out.push_str(&wrapped(
        docs.iter().map(|doc| doc.name.clone()),
        "\n  ",
        79,
    ));
    out.push_str(
        "\n\nThe server: `str NAME server` or `str PATH socket` first; else\n\
         FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux, or with\n\
         XDG_RUNTIME_DIR unset $TMPDIR/fux. Nothing is saved on its own:\n\
         `fux all dump > FILE` saves the layouts and launches when you choose\n\
         (never what a process held), and `fux start && fux --file FILE` makes\n\
         them again on a fresh server.\n",
    );
    out
}

/// `words` joined by blanks, in lines each begun with `start` and at most
/// `width` columns long, the line end included.
fn wrapped(words: impl Iterator<Item = String>, start: &str, width: usize) -> String {
    let (mut out, mut line) = (String::new(), String::from(start));
    for word in words {
        if line.len() > start.len() {
            if line.chars().count().saturating_add(word.chars().count()) >= width {
                out.push_str(&line);
                line = String::from(start);
            } else {
                line.push(' ');
            }
        }
        line.push_str(&word);
    }
    out + &line
}

/// One word at length, a topic of the guide, or `limits`.
pub fn explain(name: &str) -> Option<String> {
    let limits = (name == "limits").then(limits);
    limits.or_else(|| guide(name)).or_else(|| word(name))
}

/// One word at length: its stack effect, what it does, and an example.
fn word(name: &str) -> Option<String> {
    if let Some(word) = Word::named(name) {
        let row = word.row();
        let mut out = format!("{}  ( {} )\n\n{}.\n", row.name, row.effect, row.about);
        if let Some(usage) = crate::script::usage(name) {
            out.push_str(&format!("\n{usage}\n"));
        }
        if let Word::Act(act) = word
            && let Some(takes) = act.takes()
        {
            // What they go on: options, for what makes its own pane.
            let on = match takes.after {
                After::Opts => "opts",
                After::Pane => "the pane",
                After::Either => "the pane (or opts)",
            };
            out.push_str(&format!("\nIts options, after {on} and before {name}:\n"));
            for option in takes.options.iter().map(|o| Word::Opt(*o).row()) {
                let (name, effect, about) = (option.name, option.effect, option.about);
                out.push_str(&format!("  {name:<14} {effect:<26} {about}\n"));
            }
        }
        if word == Word::Act(Act::Set) {
            out.push_str(&settings());
        } else if word == Word::Act(Act::Json) {
            out.push_str(&format!("\n{JSON}"));
        }
        return Some(out);
    }
    if let Some(doc) = crate::script::docs().iter().find(|doc| doc.name == name) {
        let (effect, about, code) = (&doc.effect, &doc.about, doc.code.replace('\n', "\n  "));
        return Some(format!(
            "{name}  ( {effect} )\n\n{about}.\nWritten in fuxth:\n\n  {code}\n"
        ));
    }
    if let Some(builtin) = fuxth::Builtin::named(name) {
        return Some(format!(
            "{name}  ( {} )\n\n{}. One of the core's words.\n",
            builtin.effect(),
            builtin.about()
        ));
    }
    let (shape, about) = fuxth::Parsing::named(name)?.syntax();
    Some(format!("{name}  {shape}: {about}\n"))
}

/// Local settings and their bundled initialization source.
fn settings() -> String {
    let mut out = String::from("\nThe settings:\n");
    for [name, takes, about] in crate::client_state::table() {
        let about = about.split_whitespace().map(str::to_owned);
        out.push_str(&format!(
            "  {name}: {takes}{}\n",
            wrapped(about, "\n      ", 78)
        ));
    }
    out.push_str(&format!("\nShell, copy-environment, history-bytes, program-clipboard, reflow,\nalternate-scroll-lines and picture-bytes supply this client's future launches.\nUse PANE BYTES history-budget to change one pane.\nBell, titles, terminal-clipboard, cell-placement, styles, bar, bar-slots\nand show-column affect only this client's view: supplied with attach, then updated after local commits\nbefore following requests.\nWait-seconds supplies this client's observation wait limits.\nEscape-delay-ms and paste-timeout-ms change at an idle decoder boundary.\nSetters and config evaluation send no requests. Shared paste buffers\nretain the newest {} copies.\n", crate::session::BUFFER_LIMIT));
    out.push_str("\nStyles rows: [role foreground background flags...]; roles: bar tab keys\nnotice error panel border border-focus. Colours: 0..255, default, #rrggbb.\nFlags: bold dim italic underline. A subset changes only its supplied roles.\nBar: [top/bottom/off [left fields] [right fields] tab-numbers?].\nFields: workspace tabs pane [slot 0..15]; at most 16, each once.\nNative notices, copy/prefix/repeat/zoom hints and clipping remain authoritative.\nOff overlays native feedback on the bottom pane row without resizing PTYs.\nBar-slots replaces all slots: up to 16 [0..15 text] rows, 256 bytes per\ntext, no controls. Text is inert. No paint-time programs or added timers.\nstr show-column false set hides the body, retaining dispatch, Escape and feedback.\n");
    out.push_str("\nBundled initialization (client-defaults.fux):\n");
    out.push_str(crate::script::DEFAULTS);
    out
}

/// How much a bound is, as the docs write it.
#[derive(Clone, Copy, Debug)]
enum Amount {
    /// Grouped by commas: `262,144`.
    Count(usize),
    /// A count kept as a `u64`, grouped the same.
    Whole(u64),
    /// In MiB or KiB when a whole number of them, else in bytes.
    Bytes(usize),
    Time(std::time::Duration),
    /// A cell's width and height.
    Pixels((u16, u16)),
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let grouped = |digits: String| {
            let mut out = String::new();
            for (i, digit) in digits.chars().enumerate() {
                if i > 0 && digits.len().saturating_sub(i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        };
        match *self {
            Amount::Count(n) => f.write_str(&grouped(n.to_string())),
            Amount::Whole(n) => f.write_str(&grouped(n.to_string())),
            Amount::Bytes(n) if n > 0 && n.is_multiple_of(1 << 20) => write!(f, "{} MiB", n >> 20),
            Amount::Bytes(n) if n > 0 && n.is_multiple_of(1 << 10) => write!(f, "{} KiB", n >> 10),
            Amount::Bytes(n) => write!(f, "{} bytes", grouped(n.to_string())),
            Amount::Time(t) if t.subsec_nanos() == 0 => write!(f, "{} s", t.as_secs()),
            Amount::Time(t) if t.subsec_nanos().is_multiple_of(1_000_000) => {
                write!(f, "{} ms", t.as_millis())
            }
            Amount::Time(t) => write!(f, "{} ns", t.as_nanos()),
            Amount::Pixels((width, height)) => write!(f, "{width} by {height} pixels"),
        }
    }
}

/// Where the docs state a bound, and in what words: `(place, words)`.
type Said = &'static [(&'static str, &'static str)];

/// The bounds fux keeps, each from the constant its check uses: how much,
/// what it bounds, and where the docs state it (`README`, `agents`, `json`
/// or a word's name) in what words, `{}` for how much, which the docs test
/// finds there.
#[rustfmt::skip]
fn bounds() -> Vec<(Amount, &'static str, Said)> {
    use Amount::{Bytes, Count, Pixels, Time, Whole};
    use crate::{client, copy, pane, script, server};
    vec![
        (Bytes(crate::wire::MAX_REPLY), "a finite reply's encoded bytes, including all its parts", &[]),
        (Bytes(crate::wire::MAX_ALLOCATED), "a finite observation's allocation grant, shared by construction or nested decoding", &[]),
        (Count(crate::capture::OUTPUT_CELLS), "additional capture cells for synthesized output, beside twice history-bytes and the live screen", &[]),
        (Bytes(crate::protocol::MAX_FRAME), "a message between client and server", &[("README", "length-prefixed and at most {}")]),
        (Bytes(crate::session::LAUNCH_BYTES), "saved launch inputs in one session, including pending replacements", &[]),
        (Bytes(crate::session::DUMP_BYTES), "a dump recipe", &[]),
        (Count(crate::command::EnvPatch::MAX_NAMES), "environment names selected or patched in one launch", &[]),
        (Bytes(server::OUT_MAX), "what may wait unsent for a client before the copies, programs and keys sent it are refused", &[]),
        (Bytes(server::TALK_MAX), "what may wait unsent for a client before it is detached", &[]),
        (Bytes(client::OUTPUT_MAX), "what a client holds unwritten for its terminal before it reads the server no more", &[]),
        (Count(client::JOBS), "programs waiting for a client to run them", &[]),
        (Bytes(client::JOB_BYTES), "the code of the programs waiting for a client", &[]),
        (Count(client::HELD), "inputs a client holds while a program runs, and the prefix key one more", &[]),
        (Bytes(client::HELD_BYTES), "the pastes of the inputs a client holds", &[]),
        (Bytes(client::NOTICE_MAX), "a program's notice", &[]),
        (Bytes(crate::events::BACKLOG), "what an `events` stream may hold unsent before it is sent no more for now, the rest waiting in the journal", &[]),
        (Count(crate::events::KEPT), "events the journal keeps: an `events` stream further behind is told how many it missed", &[]),
        (Bytes(pane::INPUT_BYTES), "input waiting for a pane's program", &[("agents", "({} its program has not read)")]),
        (Bytes(crate::script::INPUT_MAX), "keys one send-keys sends", &[("agents", "{} at most in all"), ("send-keys", "{} at most in all")]),
        (Bytes(fux_vt::keys::decode::PASTE_LIMIT), "a paste from a client's terminal", &[]),
        (Bytes(pane::REPLIES_MAX), "replies to one read of a program's output", &[("json", "past {} of replies")]),
        (Bytes(crate::session::LEFT_MAX), "what is read of what a program left on its terminal as it ended", &[("json", "more than {} on its terminal")]),
        (Time(pane::FRAME_TIMEOUT), "how long a frame of synchronized output is held", &[("README", "held frames are released after {}")]),
        (Bytes(pane::FRAME_LIMIT), "a frame of synchronized output held", &[("README", "or past {}")]),
        (Count(pane::TITLE_MAX), "characters of a title kept", &[("json", "| `title` | what the program set (OSC 0 or 2), else `\"\"`: its first {} characters")]),
        (Count(pane::TITLE_STACK), "titles a program may push", &[]),
        (Count(pane::NOTIFICATION_MAX), "characters of a notification kept", &[("json", "| `notification` | the last notification's text, else `null`: its first {} characters")]),
        (Bytes(crate::session::NAME_MAX), "a name", &[]),
        (Count(copy::MAX_CELLS), "cells a copy takes", &[("README", "at most {} cells")]),
        (Bytes(copy::MAX_CLIPBOARD), "a copy to your clipboard, encoded", &[("README", "to {} encoded")]),
        (Bytes(copy::SEARCH_MAX), "a search in copy mode", &[]),
        (Bytes(crate::view::LINE_MAX), "a line typed at a prompt", &[]),
        (Bytes(fux_vt::OSC_PAYLOAD_LIMIT), "a string a program sends, as its copy (OSC 52)", &[("README", "up to {} encoded"), ("json", "the {} a string may be")]),
        (Whole(u64::from(fux_vt::sixel::MAX_SIDE)), "pixels a side of a sixel picture", &[("json", "{} pixels a side")]),
        (Count(fux_vt::sixel::MAX_PIXELS), "pixels of a sixel picture in all", &[("json", "{} pixels in all")]),
        (Pixels(fux_vt::CellSize::LEAST), "the least cell a terminal may say", &[("README", "{}")]),
        (Time(crate::outer::BELL_GAP), "between two bells told", &[("README", "at most once every {}")]),
        (Count(crate::session::BUFFER_LIMIT), "paste buffers kept", &[("README", "the newest {}")]),
        (Count(crate::session::PREVIEW_MAX), "characters of a paste buffer's preview", &[("README", "at most {} characters"), ("json", "at most {} characters")]),
        (Count(crate::command::MAX_WAIT), "seconds a wait may be given", &[]),
        (Whole(script::PROGRAM_STEPS), "steps of a program a client runs (a config, a key's)", &[]),
        (Whole(script::LINE_STEPS), "steps of a command line's program", &[]),
        (Count(fuxth::LONGEST), "characters of a string fuxth builds", &[("agents", "at most {} characters")]),
        (Whole(fuxth::BUILT), "elements a fuxth run builds in all", &[]),
        (Whole(fuxth::TOTAL), "elements a fuxth list holds, through the lists in it", &[]),
        (Bytes(script::SOURCE_MAX), "what read-file and input read", &[("input", "{} at most"), ("read-file", "{} at most")]),
        (Count(usize::from(crate::view::MOST)), "columns or rows of a size", &[("resize", "1 to {} each"), ("size", "1 to {} each")]),
        (Count(fuxix::process::TREE_MAX), "processes a pane's teardown looks through for its session (Linux)", &[]),
    ]
}

/// `fux help limits`: the bounds fux keeps.
fn limits() -> String {
    let mut out = String::from("The bounds fux keeps, each as its check has it:\n");
    for (amount, about, _) in bounds() {
        let about = about.split_whitespace().map(str::to_owned);
        out.push_str(&format!("  {amount}:{}\n", wrapped(about, "\n      ", 78)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Opt;

    /// Where the docs and words' help state a bound, they state it as the
    /// table of bounds has it, from the constant its check uses; the JSON
    /// they show is this protocol's.
    #[test]
    fn the_docs_state_each_bound_as_its_check_has_it() {
        // Each place's words, a blank between each, as its lines wrap them.
        let place = |at: &str| {
            let text = match at {
                "README" => include_str!("../README.md"),
                "agents" => AGENTS,
                "json" => JSON,
                word => Word::named(word).map_or("", |w| w.row().about),
            };
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        for (amount, about, said) in bounds() {
            for (at, words) in said {
                let words = words.replace("{}", &amount.to_string());
                assert!(place(at).contains(&words), "{about}: {at}: {words}");
            }
        }
        let protocol = crate::protocol::PROTOCOL;
        for said in [
            format!("\"protocol\":{protocol},"),
            format!("\"server_protocol\":{protocol},"),
            // The language's codes fux's words raise, which a try catches
            // only as fux says so.
            {
                let raised = crate::code::Raise::LANG.iter();
                let declared = raised.filter(|c| !c.raised());
                let quoted: Vec<String> = declared.map(|c| format!("`{}`", c.name())).collect();
                let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
                crate::words::listed(&quoted, "and")
            },
        ] {
            assert!(JSON.contains(&said), "json.md: {said}");
        }
        // `fux help limits` lists them all, written exactly.
        let limits = explain("limits").unwrap_or_default();
        let amounts = limits.lines().filter(|line| !line.starts_with("    "));
        assert_eq!(amounts.count(), bounds().len() + 1);
        for amount in ["  1 MiB:", "  1,048,768 bytes:", "  250 ms:", "  4,096:"] {
            assert!(limits.contains(amount), "{limits}");
        }
    }

    /// The guide's topics, as `explain` and the guide's first screen
    /// list them, are its headings, which `explain` reads; none is a word.
    #[test]
    fn the_guide_lists_its_topics_as_its_headings_are() {
        let topics = super::topics();
        assert!(topics.len() >= 5, "{topics:?}");
        let on = Word::Act(Act::Explain).row().about;
        let listed = format!("({})", topics.join(" "));
        assert!(on.contains(&listed), "{listed}");
        for topic in topics.iter().chain(&["agents", "guide"]) {
            assert!(word(topic).is_none(), "{topic} is a word");
            assert!(
                explain(topic).is_some_and(|text| AGENTS.contains(&text)),
                "{topic}"
            );
        }
        let quoted: Vec<_> = topics.iter().map(|t| format!("`{t}`")).collect();
        let listed = format!("whole guide): {}.", quoted.join(", "));
        let guide = AGENTS.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(guide.contains(&listed), "agents.md: {listed}");
    }

    /// Every word, and every word of the library, has its help; examples
    /// compile.
    #[test]
    fn every_word_has_its_help() {
        let overview = overview();
        for word in Word::ALL.iter().map(|w| w.row()) {
            assert!(overview.contains(word.name), "{}", word.name);
            assert!(explain(word.name).is_some(), "{}", word.name);
            // Each fux command line in it, a shell line's too.
            let commands = (word.example.split(['(', ')', ';', '|', '>']))
                .filter_map(|part| part.trim().strip_prefix("fux "))
                .filter(|code| !code.ends_with(".fux"));
            for code in commands {
                let tokens = fuxth::token::source("example", code);
                let compiled = tokens.map_err(|e| e.to_string()).and_then(|t| {
                    crate::script::compile(&t)
                        .map(drop)
                        .map_err(|e| e.to_string())
                });
                assert!(compiled.is_ok(), "{}: {compiled:?}", word.name);
            }
        }
        let docs = crate::script::docs();
        assert!(docs.len() >= 5);
        // A word defined over several lines shows all of them.
        let range = explain("range").unwrap_or_default();
        assert!(range.contains("  define range\n    let to\n"), "{range}");
        assert!(range.ends_with("\n  end\n"), "{range}");
        for doc in docs.iter() {
            assert!(explain(&doc.name).is_some(), "{}", doc.name);
        }
        // Each word's options go on what it takes.
        for (name, on) in [("send-keys", "after the pane"), ("run", "after opts")] {
            let explained = explain(name).unwrap_or_default();
            assert!(explained.contains(&format!("options, {on}")), "{explained}");
        }
        // A word's options go after opts just when it starts from them.
        for act in crate::script::Act::ALL {
            let (opts, row) = (act.takes().map(|t| t.after), Word::Act(*act).row());
            let from_opts = row.effect.starts_with("opts ");
            assert_eq!(opts == Some(After::Opts), from_opts, "{act}");
        }
        // Opts' own help names the words that start from them.
        let from_opts = crate::script::from_opts("and");
        assert!(Word::Act(Act::Opts).row().about.contains(&from_opts));
        assert!(overview.contains(&from_opts));
        // Each option goes on a word.
        for opt in Opt::ALL {
            assert!(!opt.takers().is_empty(), "{opt}");
        }
        assert!(AGENTS.contains("fux "));
        // `set`'s help names every setting.
        let set = explain("set").unwrap_or_default();
        for name in crate::client_state::NAMES {
            assert!(set.contains(&format!("\n  {name}: ")), "{name}");
        }
    }
}
