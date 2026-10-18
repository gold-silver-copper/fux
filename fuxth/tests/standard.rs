//! Cases from the Forth 2012 core tests (John Hayes' `tester.fr`, as
//! `T{ 1 2 + -> 3 }T`) and Exercism's Forth exercise, in fuxth's words: a
//! program and what it leaves, or the error it ends with. Where fuxth
//! chose otherwise, the case says so.
mod common;
use common::{Ended, V, fuxth};

fn ints(values: &[i64]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|n| V::Int(*n)).collect())
}

fn bools(values: &[bool]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|b| V::Bool(*b)).collect())
}

fn ends(code: &str) -> impl Fn(&Ended) -> bool + '_ {
    move |ended| matches!(ended, Ended::Failed(c, _) | Ended::Refused(c, _) if c == code)
}

#[test]
fn core_stack_words() {
    for (program, left) in [
        ("1 2 drop", &[1][..]),
        ("1 dup", &[1, 1]),
        ("1 2 over", &[1, 2, 1]),
        ("1 2 3 rot", &[2, 3, 1]),
        ("1 2 swap", &[2, 1]),
        ("1 2 nip", &[2]),
        ("1 2 3 -rot", &[3, 1, 2]),
        ("1 2 2dup", &[1, 2, 1, 2]),
        ("1 2 3 2drop", &[1]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
}

#[test]
fn core_arithmetic() {
    for (program, left) in [
        ("0 5 add", &[5][..]),
        ("-1 1 add", &[0]),
        ("0 5 sub", &[-5]),
        ("-1 -1 sub", &[0]),
        ("2 3 mul", &[6]),
        ("-3 -3 mul", &[9]),
        ("0 negate 1 negate -1 negate", &[0, -1, 1]),
        ("0 abs 1 abs -1 abs", &[0, 1, 1]),
        ("0 1 min 0 -1 min", &[0, -1]),
        ("0 1 max 0 -1 max", &[1, 0]),
        // Symmetric division (Forth's SM/REM): the quotient truncates toward
        // zero, and the remainder takes the dividend's sign. Forth leaves
        // `/` to each system; gforth's floors (-7 2 / is -4).
        ("7 2 div 7 2 mod", &[3, 1]),
        ("-7 2 div -7 2 mod", &[-3, -1]),
        ("7 -2 div 7 -2 mod", &[-3, 1]),
        ("-7 -2 div -7 -2 mod", &[3, -1]),
        ("-9223372036854775808 -1 mod", &[0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    // Where Forth wraps around, fuxth says so.
    for program in [
        "9223372036854775807 1 add",
        "-9223372036854775808 -1 div",
        "-9223372036854775808 negate",
    ] {
        assert!(ends("overflow")(&fuxth(program, u64::MAX)), "{program}");
    }
    assert!(ends("div_by_zero")(&fuxth("1 0 div", u64::MAX)));
    assert!(ends("div_by_zero")(&fuxth("1 0 mod", u64::MAX)));
}

#[test]
fn core_comparisons() {
    // Booleans are their own kind, not -1 and 0.
    for (program, left) in [
        (
            "0 0 eq 1 1 eq -1 -1 eq 1 0 eq",
            &[true, true, true, false][..],
        ),
        (
            "0 1 lt 1 2 lt -1 0 lt 0 0 lt 1 0 lt",
            &[true, true, true, false, false],
        ),
        ("0 1 gt 1 0 gt 0 0 gt", &[false, true, false]),
        ("true not false not", &[false, true]),
        ("1 2 ne 2 2 le 3 2 ge", &[true, true, true]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), bools(left), "{program}");
    }
    // `0 0 eq 1 add` is -1 + 1 in Forth; a boolean isn't an integer here.
    assert!(ends("type")(&fuxth("0 0 eq 1 add", u64::MAX)));
    assert_eq!(fuxth("true 0 eq", u64::MAX), bools(&[false]));
}

#[test]
fn core_control_and_definitions() {
    let gi1 = "define gi1 if 123 then end";
    let gi2 = "define gi2 if 123 else 234 then end";
    for (program, left) in [
        (format!("{gi1} false gi1 true gi1"), &[123][..]),
        (format!("{gi2} false gi2 true gi2"), &[234, 123]),
        // T{ : GI3 BEGIN DUP 5 < WHILE DUP 1+ REPEAT ; }T, as until.
        (
            "define gi3 begin dup 1 add dup 5 lt not until end 0 gi3".to_owned(),
            &[0, 1, 2, 3, 4, 5],
        ),
        // RECURSE is a word's own name here.
        (
            "define gi4 dup 1 gt if dup 1 sub gi4 mul then end 5 gi4".to_owned(),
            &[120],
        ),
        ("123 let x123 x123".to_owned(), &[123]),
        ("define nop end 1 nop".to_owned(), &[1]),
    ] {
        assert_eq!(fuxth(&program, u64::MAX), ints(left), "{program}");
    }
}

/// Exercism's cases, with where fuxth parts from them.
#[test]
fn exercism_forth() {
    for (program, left) in [
        ("1 2 3 4 5", &[1, 2, 3, 4, 5][..]),
        ("-1 -2 -3 -4 -5", &[-1, -2, -3, -4, -5]),
        ("1 2 add 4 sub 3 mul 2 div", &[-1]),
        ("define dup-twice dup dup end 1 dup-twice", &[1, 1, 1]),
        ("define countup 1 2 3 end countup", &[1, 2, 3]),
        ("define foo 5 end define bar foo end bar", &[5]),
        // A definition may take a library word's name: from there on.
        ("define swap dup end 1 swap", &[1, 1]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    for (program, code) in [
        ("drop", "stack_underflow"),
        ("1 swap", "stack_underflow"),
        ("foo", "unknown_word"),
        ("define 1 2 end", "bad_name"),
        // Exercism lets a definition replace any word; here the core's
        // are kept, and a program defines a name once.
        ("define drop dup end", "bad_name"),
        ("define foo 5 end define foo 6 end", "bad_name"),
    ] {
        assert!(
            ends(code)(&fuxth(program, u64::MAX)),
            "{program}: {:?}",
            fuxth(program, u64::MAX)
        );
    }
    // Names are as written: `DUP` is no word, and `Dup` none either.
    assert!(ends("unknown_word")(&fuxth("1 DUP", u64::MAX)));
}
