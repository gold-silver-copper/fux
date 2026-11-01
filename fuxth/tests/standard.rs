//! Cases from the Forth 2012 core tests (John Hayes' `tester.fr`, as
//! `T{ 1 2 + -> 3 }T`, Annex F) and Exercism's Forth exercise, in fuxth's
//! words, translated through the plan's table: a program and what it
//! leaves, or the error it ends with. Where fuxth chose otherwise (§2 of
//! the plan: typed cells), the case says so.
mod common;
use common::{Ended, Toy, V, fuxth};
use fuxth::{Machine, Outcome, STDLIB, Value, Vocabulary, token};

fn ints(values: &[i64]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|n| V::Int(*n)).collect())
}

fn bools(values: &[bool]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|b| V::Bool(*b)).collect())
}

fn mixed(values: &[V]) -> Ended {
    Ended::Left(Vec::new(), values.to_vec())
}

fn ends(code: &str) -> impl Fn(&Ended) -> bool + '_ {
    move |ended| matches!(ended, Ended::Failed(c, _) | Ended::Refused(c, _) if c == code)
}

#[test]
fn core_stack_words() {
    for (program, left) in [
        ("1 2 3 4 5", &[1, 2, 3, 4, 5][..]),
        ("-1 -2 -3 -4 -5", &[-1, -2, -3, -4, -5]),
        ("1 2 drop", &[1]),
        ("1 dup", &[1, 1]),
        ("1 2 over", &[1, 2, 1]),
        ("1 2 3 rot", &[2, 3, 1]),
        ("1 2 swap", &[2, 1]),
        ("1 2 nip", &[2]),
        ("1 2 3 -rot", &[3, 1, 2]),
        ("1 2 2dup", &[1, 2, 1, 2]),
        ("1 2 3 2drop", &[1]),
        ("1 2 swap over dup drop", &[2, 1, 2]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
}

#[test]
fn core_arithmetic() {
    for (program, left) in [
        ("0 5 add", &[5][..]),
        ("1 2 add 4 sub 3 mul 2 div", &[-1]),
        ("-1 1 add", &[0]),
        ("0 5 sub", &[-5]),
        ("-1 -1 sub", &[0]),
        ("2 3 mul", &[6]),
        ("-3 -3 mul", &[9]),
        ("10 3 sub 2 mul 5 div 7 3 mod", &[2, 1]),
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
        ("-9223372036854775808 -1 mod 7 -1 mod", &[0, 0]),
        // MAX-INT 1+ -> MIN-INT: past 64 bits, it wraps around, as Forth's
        // two's-complement cells do.
        ("9223372036854775807 1 add", &[i64::MIN]),
        ("-9223372036854775808 1 sub", &[i64::MAX]),
        ("-9223372036854775808 -1 div", &[i64::MIN]),
        ("-9223372036854775808 negate", &[i64::MIN]),
        ("4294967296 4294967296 mul", &[0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
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
    for (program, left) in [
        ("define gi1 if 123 then end false gi1 true gi1", &[123][..]),
        (
            "define gi2 if 123 else 234 then end false gi2 true gi2",
            &[234, 123],
        ),
        // T{ : GI3 BEGIN DUP 5 < WHILE DUP 1+ REPEAT ; }T, as until.
        (
            "define gi3 begin dup 1 add dup 5 lt not until end 0 gi3",
            &[0, 1, 2, 3, 4, 5],
        ),
        (
            "define gi4 dup 1 gt if dup 1 sub recurse mul then end 5 gi4",
            &[120],
        ),
        ("123 let x123 x123", &[123]),
        ("6 let six six six mul", &[36]),
        ("define nop end 1 nop", &[1]),
        ("define dup-twice dup dup end 1 dup-twice", &[1, 1, 1]),
        ("define countup 1 2 3 end countup", &[1, 2, 3]),
        ("define foo 5 end define bar foo end bar", &[5]),
        ("define swap dup end 1 swap", &[1, 1]),
        ("define twice dup add end 5 twice twice", &[20]),
        ("0 begin 1 add dup 5 eq until", &[5]),
        (
            "define countdown begin dup 1 sub dup 0 eq until end 3 countdown",
            &[3, 2, 1, 0],
        ),
        ("1 2 lt if 10 else 20 then", &[10]),
        ("2 1 lt if 10 else 20 then", &[20]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
}

/// The flags `if` and `until` take: Forth's are integers, and fuxth takes
/// an integer too, true unless it is 0; its comparisons give booleans.
#[test]
fn core_flags() {
    for (program, left) in [
        ("-1 if 1 else 2 then 0 if 3 else 4 then", &[1, 4][..]),
        ("5 begin 1 sub dup 0 eq until", &[0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    assert_eq!(fuxth("0 not 7 not", u64::MAX), bools(&[true, false]));
}

/// ' EXECUTE FIND IMMEDIATE LITERAL POSTPONE STATE [ ] :NONAME RECURSE
/// EXIT VALUE TO CHAR [CHAR], as `tick execute find immediate literal
/// postpone compiling stop-compiling start-compiling code … end recurse
/// leave-definition let to first`. Deviations: `find` gives true for an
/// immediate word too (Forth gives 1 or -1); `compiling` gives a boolean;
/// GI6 leaves its numbers counting down, as it has no return stack yet.
#[test]
fn core_dictionary_and_compiling() {
    let gt = "define gt1 123 end define gt2 tick gt1 end immediate
        define gt4 postpone gt1 end immediate define gt5 gt4 end
        define gt6 345 end immediate define gt7 postpone gt6 end
        define gt8 compiling end immediate ";
    for (program, left) in [
        // T{ ' GT1 EXECUTE -> 123 }T  T{ GT2 EXECUTE -> 123 }T
        ("tick gt1 execute", &[V::Int(123)][..]),
        ("gt2 execute", &[V::Int(123)]),
        // T{ GT1STRING FIND -> ' GT1 -1 }T  T{ GT2STRING FIND -> ' GT2 1 }T
        (
            "str gt1 find swap tick gt1 eq",
            &[V::Bool(true), V::Bool(true)],
        ),
        (
            "str gt2 find swap tick gt2 eq",
            &[V::Bool(true), V::Bool(true)],
        ),
        // T{ : GT3 GT2 LITERAL ; -> }T  T{ GT3 -> ' GT1 }T
        (
            "define gt3 gt2 literal end gt3 tick gt1 eq",
            &[V::Bool(true)],
        ),
        // T{ GT5 -> 123 }T  T{ GT7 -> 345 }T
        ("gt5", &[V::Int(123)]),
        ("gt7", &[V::Int(345)]),
        // T{ GT8 -> 0 }T  T{ : GT9 GT8 LITERAL ; -> }T  T{ GT9 0= -> <FALSE> }T
        (
            "gt8 define gt9 gt8 literal end gt9",
            &[V::Bool(false), V::Bool(true)],
        ),
        // T{ : GC5 [ 1 2 + ] LITERAL ; GC5 -> 3 }T
        (
            "define gc5 stop-compiling 1 2 add start-compiling literal end gc5",
            &[V::Int(3)],
        ),
        // T{ :NONAME 1234 ; EXECUTE -> 1234 }T
        ("code 1234 end execute", &[V::Int(1234)]),
        // T{ 0 GI6 -> 0 }T  T{ 4 GI6 -> 0 1 2 3 4 }T, counting down here.
        (
            "define gi6 dup if dup 1 sub recurse then end 0 gi6",
            &[V::Int(0)],
        ),
        (
            "define gi6 dup if dup 1 sub recurse then end 4 gi6",
            &[V::Int(4), V::Int(3), V::Int(2), V::Int(1), V::Int(0)],
        ),
        // T{ : GE1 1 EXIT 2 ; GE1 -> 1 }T
        ("define ge1 1 leave-definition 2 end ge1", &[V::Int(1)]),
        // T{ 111 VALUE V1 -> }T  T{ V1 -> 111 }T  T{ 222 TO V1 -> }T  T{ V1 -> 222 }T
        ("111 let v1 v1 222 to v1 v1", &[V::Int(111), V::Int(222)]),
        // T{ : VD2 TO V2 ; -> }T  T{ 333 VD2 -> }T  T{ V2 -> 333 }T
        ("222 let v2 define vd2 to v2 end 333 vd2 v2", &[V::Int(333)]),
        // T{ CHAR X -> 58 }T  T{ : GC1 [CHAR] X ; GC1 -> 58 }T
        (
            "str X first define gc1 str X first end gc1",
            &[V::Int(88), V::Int(88)],
        ),
    ] {
        let program = format!("{gt}{program}");
        assert_eq!(fuxth(&program, u64::MAX), mixed(left), "{program}");
    }
}

/// EVALUATE, where the host enables it: T{ GE1 EVALUATE -> 123 }T, and a
/// definition made by the text evaluated (T{ GE3 ... }T).
#[test]
fn core_evaluate() -> Result<(), fuxth::Error> {
    let mut words = Vocabulary::default();
    words.evaluate();
    let mut machine = Machine::new(words);
    machine.library("stdlib.fux", STDLIB)?;
    let program = "define ge1 str 123 end immediate ge1 evaluate
        str 'define ge3 456 end' evaluate ge3
        define ge4 str 'ge1 evaluate' evaluate end ge4";
    machine.read(token::source("program", program)?, None);
    let ran = machine.run(&mut Toy::default(), u64::MAX)?;
    assert_eq!(ran, Outcome::Finished);
    let left = [123, 456, 123].map(Value::Int);
    assert_eq!(machine.stack(), left.as_slice());
    Ok(())
}

/// EMIT and TYPE, as `emit` and `write`: T{ 65 EMIT -> }T prints A.
#[test]
fn core_output() {
    let ended = fuxth("65 emit str BC write 10 emit str D print", u64::MAX);
    let printed = match ended {
        Ended::Left(printed, _) => printed.concat(),
        other @ (Ended::Failed(..) | Ended::Refused(..) | Ended::Endless) => format!("{other:?}"),
    };
    assert_eq!(printed, "ABC\nD\n");
}
