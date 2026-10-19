//! vt-houdini's reply vectors, re-authored. vt-houdini
//! (<https://github.com/dgl/vt-houdini>, commit 314cf694, AGPL-3.0, so
//! nothing of it is copied) asks an ssh client's terminal a list of
//! questions and reports what it answers, and whether a CVE's payload comes
//! back: its probe and its timeout's recovery (`sshServer` and `trySeqs` in
//! `cmd/vtest-server/ssh.go`), `basicTests` (eleven) and `cveTests` (seven,
//! and a padding one with no bytes). Each vector here is its bytes again,
//! with the reply the authority gives (DEC STD 070, then ctlseqs and xterm
//! 411's source), never fux-vt's own output.
//!
//! Where vt-houdini's judgement and the authority part, the authority wins:
//! - vt-houdini warns of any DECRQCRA reply but `0000` (a program can read
//!   the screen by checksums). DEC STD 070 (DECRQCRA, 5-180) defines the
//!   reply as the rectangle's checksum, so with `RectangleChecksums` on the
//!   answer is the checksum; the risk is met by the default, which, as
//!   xterm 411's (`GetChecksum` in `disallowedWindowOps`), does not answer.
//! - It marks `CSI ? 6 n` "XXX?"; DEC STD 070 defines it (DECXCPR, 5-55).
//!
//! What the authority leaves to fux-vt, it says so too: DECREQTPARM is
//! answered by "xterm emulating VT100" (ctlseqs), and without an identity
//! fux-vt answers DA as a VT100 but leaves DECREQTPARM unanswered
//! (CONFORMANCE.md: not implemented). So bare, either is accepted, no
//! reply or ctlseqs' VT100 one; as a level-2 terminal there is none, as
//! xterm 411 answers only below a VT220 (`terminal_id < 200`).

use fux_vt::{Feature, Identity, Options, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Who the parser with every feature on answers as.
const IDENTITY: Identity = Identity {
    name: "fux",
    version: "0.10.0",
};

/// A vector: its name in vt-houdini, its bytes, and the replies the
/// authority accepts, each the whole of what the parser sends, under fux's
/// default policy and with every feature on and [`IDENTITY`].
struct Vector {
    name: &'static str,
    bytes: &'static [u8],
    default: &'static [&'static [u8]],
    every: &'static [&'static [u8]],
}

/// No reply.
const NONE: &[&[u8]] = &[b""];

/// The answer to CPR at the home position: DEC STD 070, CPR (5-53),
/// `CSI Pl ; Pc R`, the active line and column, one-based.
const CPR_HOME: &[&[u8]] = &[b"\x1b[1;1R"];

const VECTORS: &[Vector] = &[
    // The probe, before the tests: DSR 6 is CPR (DEC STD 070, 5-53).
    Vector {
        name: "probe",
        bytes: b"\x1b[6n",
        default: CPR_HOME,
        every: CPR_HOME,
    },
    // The recovery after a timeout: SGR 0, an empty OSC, then CPR. Only
    // CPR answers.
    Vector {
        name: "timeout",
        bytes: b"\x1b[m\x1b];\x1b\\\x1b[6n",
        default: CPR_HOME,
        every: CPR_HOME,
    },
    // ENQ, then DEL: DEC STD 070 D.3 sends the answerback buffer, "empty by
    // factory default"; ctlseqs: "Default response is an empty string".
    Vector {
        name: "ENQ",
        bytes: b"\x05\x7f",
        default: NONE,
        every: NONE,
    },
    Vector {
        name: "DSR",
        bytes: b"\x1b[6n",
        default: CPR_HOME,
        every: CPR_HOME,
    },
    // DECXCPR (DEC STD 070, 5-55): `CSI ? Pl ; Pc ; Pp R`, "default Pl,
    // Pc, Pp: 1", so the page left out is page 1 (xterm 411 leaves it out
    // below a VT330). An opt-in (`ExtendedReplies`).
    Vector {
        name: "DSR??",
        bytes: b"\x1b[?6n",
        default: NONE,
        every: &[b"\x1b[?1;1R", b"\x1b[?1;1;1R"],
    },
    // DECREQTPARM: see the module's note.
    Vector {
        name: "DECREQTPARM",
        bytes: b"\x1b[x",
        default: &[b"", b"\x1b[2;1;1;128;128;1;0x"],
        every: NONE,
    },
    // Primary DA (DEC STD 070, DA; ctlseqs' codes): bare, a VT100 with the
    // advanced video option (`? 1 ; 2`); with an identity, level 2 (62)
    // with selective erase (6), horizontal scrolling (21), colour text
    // (22) and rectangular editing (28), no sixel (4) as no host shows it.
    Vector {
        name: "DA",
        bytes: b"\x1b[c",
        default: &[b"\x1b[?1;2c"],
        every: &[b"\x1b[?62;6;21;22;28c"],
    },
    // Secondary DA (ctlseqs: `CSI > Pp ; Pv ; Pc c`, Pp 1 a VT220, Pv the
    // firmware version, Pc 0): 0.10.0 in base 100 is 1000. An opt-in.
    Vector {
        name: "DA2",
        bytes: b"\x1b[>c",
        default: NONE,
        every: &[b"\x1b[>1;1000;0c"],
    },
    // Tertiary DA is level 4 (DEC STD 070, DA3, 4-26); xterm 411 answers
    // it only at `vtXX_level >= 4`. fux-vt is level 2 at most.
    Vector {
        name: "DA3",
        bytes: b"\x1b[=c",
        default: NONE,
        every: NONE,
    },
    // SCOSC, an X at home, DECRQCRA of that one cell, SCORC. DECCKSR
    // (DEC STD 070, 5-182) is `DCS Pid ! ~ D..D ST`, four hex digits; the
    // VT520's sum, as xterm 411's `xtermCheckRect` takes it, is the
    // negated sum of the cells, 'X' (0x58) with no attribute: 0x10000 -
    // 0x58 is FFA8. Off by default, as xterm 411's.
    Vector {
        name: "DECRQCRA",
        bytes: b"\x1b[s\x1b[HX\x1b[1;1;1;1;1;1*y\x1b[u",
        default: NONE,
        every: &[b"\x1bP1!~FFA8\x1b\\"],
    },
    // XTVERSION (ctlseqs: `DCS > | text ST`), an identity's name and
    // version. Without one there is no name to give.
    Vector {
        name: "XTVERSION",
        bytes: b"\x1b[>q",
        default: NONE,
        every: &[b"\x1bP>|fux 0.10.0\x1b\\"],
    },
    // Report the window title (ctlseqs: `OSC l label ST`), which xterm
    // 411 refuses by default (`GetWinTitle` in `disallowedWindowOps`,
    // `allowWindowOps` false): CVE-2003-0063. fux-vt keeps no title.
    Vector {
        name: "TITLE",
        bytes: b"\x1b[21t",
        default: NONE,
        every: NONE,
    },
    // DECRQSS of the pen (ctlseqs: `DCS 1 $ r Pt ST` for a valid request,
    // Pt the sequence that sets it): the initial pen is SGR 0. An opt-in.
    Vector {
        name: "DECRQSS_SGR",
        bytes: b"\x1bP$qm\x1b\\",
        default: NONE,
        every: &[b"\x1bP1$r0m\x1b\\"],
    },
    // The CVEs: each sets or sends a payload and asks for it back.
    // A title, then its report (CVE-2003-0063): as TITLE.
    Vector {
        name: "CVE-2003-0063",
        bytes: b"\x1b]0;touch /tmp/cve-2003-0063\x07\x1b[21t",
        default: NONE,
        every: NONE,
    },
    // DECRQSS of no setting: ctlseqs, `DCS 0 $ r ST`, nothing echoed.
    Vector {
        name: "CVE-2008-2383",
        bytes: b"\x1bP$q;touch /tmp/cve-2008-2383\x1b\\",
        default: NONE,
        every: &[b"\x1bP0$r\x1b\\"],
    },
    // XTGETTCAP of a name that is not hex: ctlseqs, `DCS 0 + r ST` "for
    // invalid requests"; xterm 411's `x_decode_hex` stops at once at the
    // `o`, so nothing is echoed. Answered with `SettingReports`.
    Vector {
        name: "CVE-2019-0542",
        bytes: b"\x1bP+qfoo;\ntouch /tmp/cve-2019-0542;aa\n\x1b\\",
        default: NONE,
        every: &[b"\x1bP0+r\x1b\\"],
    },
    // ESC G is SSA (ECMA-48 8.3.138, 04/07 in its 7-bit form), with no
    // reply; rxvt-unicode read it as a graphics query.
    Vector {
        name: "CVE-2021-33477",
        bytes: b"\x1bG",
        default: NONE,
        every: NONE,
    },
    // xterm's font OSC 50, set then queried: fux-vt presents one font and
    // leaves it to the host (CONFORMANCE.md's presentation rows).
    Vector {
        name: "CVE-2022-45063",
        bytes: b"\x1b]50;$(touch /tmp/cve-2022-45063)\x07\x1b]50;?\x07",
        default: NONE,
        every: NONE,
    },
    // A title with CRs in it, then its report: as TITLE.
    Vector {
        name: "CVE-2022-46387",
        bytes: b"\x1b]0;\rtouch /tmp/cve-2022-46387\r\x07\x1b[21t",
        default: NONE,
        every: NONE,
    },
    // DECRQSS of no setting, with a newline in it, then of the pen: the
    // first invalid, the second the pen (ctlseqs, as above).
    Vector {
        name: "CVE-2022-45872",
        bytes: b"\x1bP$q;touch /tmp/cve-2022-45872\n\x1b\\\n\x1bP$qm\x1b\\",
        default: NONE,
        every: &[b"\x1bP0$r\x1b\\\x1bP1$r0m\x1b\\"],
    },
];

/// What `bytes` make a fresh 25 by 80 parser with `options` send back.
fn replies(options: Options, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut parser = Parser::with_options(Size::new(25, 80)?, 0, options)?;
    let mut out = Vec::new();
    parser.process_with_replies(bytes, |reply| out.extend_from_slice(reply))?;
    Ok(out)
}

/// Every vector's replies, against `expected`, as one list of misses.
fn misses(options: Options, expected: fn(&Vector) -> &'static [&'static [u8]]) -> Result<String> {
    let mut misses = String::new();
    for vector in VECTORS {
        let got = replies(options, vector.bytes)?;
        if !expected(vector).contains(&got.as_slice()) {
            misses.push_str(&format!(
                "{}: {:?}, not one of {:?}\n",
                vector.name,
                String::from_utf8_lossy(&got),
                expected(vector)
                    .iter()
                    .map(|reply| String::from_utf8_lossy(reply))
                    .collect::<Vec<_>>()
            ));
        }
    }
    Ok(misses)
}

/// Every feature on, answering as [`IDENTITY`].
fn every() -> Options {
    Feature::ALL
        .into_iter()
        .collect::<Options>()
        .with_identity(Some(IDENTITY))
}

#[test]
fn fuxs_default_policy_answers_vt_houdini_as_the_authority_says() -> Result {
    assert_eq!(misses(Options::default(), |v| v.default)?, "");
    Ok(())
}

#[test]
fn every_reply_feature_answers_vt_houdini_as_the_authority_says() -> Result {
    assert_eq!(misses(every(), |v| v.every)?, "");
    Ok(())
}

/// vt-houdini's verdict on the CVEs: a reply holding `cve` gave the
/// payload back. None may, whatever is on.
#[test]
fn no_reply_gives_a_cve_payload_back() -> Result {
    for options in [Options::default(), every()] {
        for vector in VECTORS {
            let got = replies(options, vector.bytes)?;
            assert!(
                !(0..got.len())
                    .any(|at| got.get(at..).is_some_and(|tail| tail.starts_with(b"cve"))),
                "{}: {:?}",
                vector.name,
                String::from_utf8_lossy(&got)
            );
        }
    }
    Ok(())
}
