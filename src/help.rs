//! fux's help, made from its words (`crate::script::Word`) and the bundled
//! library's comments, so that it always says what this binary does; and
//! the guide to driving programs from a script or an agent
//! (`str agents explain`), which is `docs/agents.md`, built in.
use crate::script::{Act, After, LIBRARY, Word};

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");

/// Of the guide, `agents` its first screen (all before its first topic),
/// `guide` all of it, else the topic's section, headed `## TOPIC:`; `None`
/// if it has none so named.
fn guide(topic: &str) -> Option<String> {
    match topic {
        "agents" => {
            return AGENTS
                .split_once("\n## ")
                .map(|(first, _)| first.to_owned());
        }
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

/// What `json` gives, word by word (`str json explain`).
pub const JSON: &str = include_str!("../docs/json.md");

/// How fux runs, and every word under its heading.
pub fn overview() -> String {
    let (wait, shebang) = (
        crate::config::Limit::DEFAULT.seconds(),
        crate::script::SHEBANG,
    );
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
text send-keys`, `%2 all-history capture`); only run, new-tab and
new-workspace start from opts (`opts str make program str test arg
run`), laid out on a size given (`opts 80 24 size …`), else the
caller's own screen or tab (`fux` run in a pane): run from outside fux,
a size must be given, as fux makes none up. An action answers at once,
with its pane's version from before it acted, one word
(`%2:0.57.0.3.0:RUN`); `str V changed wait` waits in V's pane until its
version differs from V, else answers `timeout` after {wait}s unless
`SECS timeout` says (the server's wait-seconds). A failure prints
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

Below, x is a pane, tab, workspace or client (an action's answer is its
pane), a request on one, or opts; target, subject and place a pane (or a
tab, workspace or client) or a request on one; handle a pane, tab or
workspace; ref a name or a handle; text a string, number or boolean.
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
    let names = library().into_iter().map(|(name, ..)| name.to_owned());
    out.push_str(&wrapped(names, "\n  ", 79));
    out.push_str(
        "\n\nThe server: `str NAME server` or `str PATH socket` first; else\n\
         FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux, or with\n\
         XDG_RUNTIME_DIR unset $TMPDIR/fux. Nothing is saved on its own:\n\
         `fux dump > FILE` saves the layouts and launches when you choose\n\
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

/// The prelude's and library's words: each name, stack effect, what it
/// does, and its definition, from the comment before its `define`:
/// `# pane text -- : what it does`. A definition over several lines is
/// its indented body and the `end` closing it.
pub fn library() -> Vec<(&'static str, &'static str, &'static str, String)> {
    let mut words: Vec<(_, _, _, String)> = Vec::new();
    let mut comment = None;
    let mut open = false;
    for line in fuxth::STDLIB.lines().chain(LIBRARY.lines()) {
        if open && (line.starts_with("  ") || line == "end") {
            if let Some((.., code)) = words.last_mut() {
                code.push_str("\n  ");
                code.push_str(line);
            }
            open = line != "end";
            continue;
        }
        open = false;
        if let Some(text) = line.strip_prefix("# ") {
            comment = text.split_once(" : ");
        } else if let Some(rest) = line.strip_prefix("define ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if let Some((effect, about)) = comment.take() {
                words.push((name, effect, about, line.to_owned()));
                open = true;
            }
        }
    }
    words
}

/// One word at length, or a topic of the guide.
pub fn explain(name: &str) -> Option<String> {
    guide(name).or_else(|| word(name))
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
    if let Some((name, effect, about, code)) = library().into_iter().find(|(n, ..)| *n == name) {
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

/// Every setting `set` takes: its name, what value it takes, its default,
/// and what it does, from `config`'s table.
fn settings() -> String {
    let mut out = String::from("\nThe settings:\n");
    for [name, takes, default, about] in crate::config::table() {
        let about = format!("{about} By default {default}.");
        let about = about.split_whitespace().map(str::to_owned);
        out.push_str(&format!(
            "  {name}: {takes}{}\n",
            wrapped(about, "\n      ", 78)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Opt;

    /// Where the docs and words' help state a bound, they state the
    /// constant the check uses; the JSON they show is this protocol's.
    #[test]
    fn the_docs_state_each_bound_as_its_check_has_it() {
        // Thousands grouped by commas, as the docs write them.
        let grouped = |n: usize| {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, digit) in digits.chars().enumerate() {
                if i > 0 && digits.len().saturating_sub(i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        };
        // A size in KiB or MiB is one only if it is a whole number of them.
        let whole = |n: usize, shift: u32| {
            assert_eq!(n % (1 << shift), 0, "{n} in units of {}", 1 << shift);
            n >> shift
        };
        // Its words, a blank between each, as its lines wrap them.
        let readme = include_str!("../README.md").split_whitespace();
        let readme = readme.collect::<Vec<_>>().join(" ");
        let (least_width, least_height) = fux_vt::CellSize::LEAST;
        let bell_gap = crate::outer::BELL_GAP.as_millis();
        for said in [
            format!("{least_width} by {least_height} pixels"),
            format!("at most {} cells", grouped(crate::copy::MAX_CELLS)),
            format!("to {} MiB encoded", whole(crate::copy::MAX_CLIPBOARD, 20)),
            format!("up to {} KiB encoded", whole(fux_vt::OSC_PAYLOAD_LIMIT, 10)),
            format!("at most {} MiB", whole(crate::protocol::MAX_FRAME, 20)),
            format!("past {} MiB", whole(crate::pane::FRAME_LIMIT, 20)),
            format!("at most once every {bell_gap} ms"),
        ] {
            assert!(readme.contains(&said), "README: {said}");
        }
        // Said in words: "after one second", "about a second after".
        let second = std::time::Duration::from_secs(1);
        assert_eq!(crate::pane::FRAME_TIMEOUT, second);
        assert!(readme.contains("shown anyway after one second"));
        let longest = format!("at most {} characters", grouped(fuxth::LONGEST));
        assert!(AGENTS.contains(&longest), "agents.md: {longest}");
        let protocol = crate::protocol::PROTOCOL;
        for said in [
            format!("\"protocol\":{protocol},"),
            format!("\"server_protocol\":{protocol},"),
            format!("its first {} characters", crate::pane::TITLE_MAX),
            format!("its first {} characters", crate::pane::NOTIFICATION_MAX),
            format!(
                "more than {} MiB on its terminal",
                whole(crate::session::LEFT_MAX, 20)
            ),
            format!("past {} bytes of replies", crate::pane::REPLIES_MAX),
            format!(
                "the {} KiB a string may be",
                whole(fux_vt::OSC_PAYLOAD_LIMIT, 10)
            ),
            format!(
                "{} pixels a side, {} pixels in all",
                grouped(usize::try_from(fux_vt::sixel::MAX_SIDE).unwrap_or_default()),
                grouped(fux_vt::sixel::MAX_PIXELS)
            ),
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
        let read = format!("{} MiB at most", whole(crate::script::SOURCE_MAX, 20));
        for word in [Act::Input, Act::ReadFile].map(Word::Act) {
            assert!(word.row().about.contains(&read), "{word:?}");
        }
        let sides = format!("1 to {} each", crate::view::MOST);
        let sized = [Act::Resize, Act::ResizeClient].map(Word::Act);
        for word in sized.into_iter().chain([Word::Opt(Opt::Size)]) {
            assert!(word.row().about.contains(&sides), "{word:?}");
        }
    }

    /// The guide's topics, as `explain` takes them.
    fn agents_topics() -> Vec<&'static str> {
        (AGENTS.split("\n## ").skip(1))
            .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
            .collect()
    }

    /// The guide's topics, as `explain` and the guide's first screen
    /// list them, are its headings, which `explain` reads; none is a word.
    #[test]
    fn the_guide_lists_its_topics_as_its_headings_are() {
        let topics = agents_topics();
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
        assert!(library().len() >= 5);
        // A word defined over several lines shows all of them.
        let range = explain("range").unwrap_or_default();
        assert!(range.contains("  define range\n    let to\n"), "{range}");
        assert!(range.ends_with("\n  end\n"), "{range}");
        for (name, ..) in library() {
            assert!(explain(name).is_some(), "{name}");
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
        // Each option goes on a word.
        for opt in Opt::ALL {
            assert!(!opt.takers().is_empty(), "{opt}");
        }
        assert!(AGENTS.contains("fux "));
        // `set`'s help names every setting.
        let set = explain("set").unwrap_or_default();
        for name in crate::config::NAMES {
            assert!(set.contains(&format!("\n  {name}: ")), "{name}");
        }
    }
}
