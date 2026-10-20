//! XParseColor's device-independent colour spaces (`rgbi:`, `CIEXYZ:`,
//! `CIEuvY:`, `CIExyY:`, `CIELab:`, `CIELuv:` and `TekHVC:`), which
//! ctlseqs's OSC 4, 5 and 10 to 19 name for a colour ("as per
//! XParseColor"), read as libX11's Xcms reads them on a screen with no
//! colour characterization of its own (no `XDCCC_LINEAR_RGB_*`
//! properties, as on Xvfb and most X servers): its default screen, a
//! Tektronix 19" monitor (libX11 1.8.10, `src/xcms/LRGB.c`), 8 bits a
//! channel, and its default gamut compression, which lowers the TekHVC
//! chroma until the colour fits (`XcmsTekHVCClipC`).
//!
//! The arithmetic is Xcms's own, step for step, its square root, cube
//! root, sine, cosine and arc tangent included (`cmsMath.c`,
//! `cmsTrig.c`): a colour on the gamut's edge lands on one side of a
//! channel's zero or the other by the last bits of a double, which the
//! platform's own functions round otherwise. Xcms's quirks are kept with
//! it: CIELab's `a*` divided by 5 rather than 500, and so on. The
//! numbers are what `%lf` reads in decimal, with Xcms's retry that reads
//! a comma for the point when the point fails (for a locale's comma).
//! Departures, each where Xlib reads garbage or hangs: `%lf`'s `inf`,
//! `nan` and hex forms, and a number that is not finite, are not read
//! (Xlib's range checks let NaN through); an `rgbi:` channel past 1 is not
//! read (Xlib's table lookup runs off the table's end and wraps to 16
//! bits: `rgbi:2/0/0` is red `6a6a` there); one below 0 is 0, as in Xlib.
//!
//! The work is counted and bounded. Each step of the arithmetic, a Newton
//! step of a square or cube root, a step of the arc tangent's
//! arithmetic-geometric mean or a step of the chroma search, is a
//! `colours` unit, spent from the call's budget (`budget.rs`). The loops
//! Xcms runs to convergence stop after a count that changes no answer:
//! Newton's square root after 600 steps (every positive double takes at
//! most 541), its cube root after 64 (its arguments are 0.008856 to 1,
//! at most 11), the mean after 64 (at most 27 for every finite argument
//! sampled; from 32 on, the answer is Xcms's 10,000 steps' bit for bit);
//! the chroma search is Xcms's, 100 steps at most. One OSC spends at most
//! an allowance (`palette.rs`): a specification that reaches it in the
//! chroma search is unreadable, at most one step past it.

// Xcms's π and its fractions (`cmsTrig.c`, `M_PI`), written to more
// digits than a double holds: these are the doubles they round to.
use std::f64::consts::{FRAC_PI_2 as HALF_PI, FRAC_PI_4 as FOURTH_PI, PI, TAU as TWO_PI};

thread_local! {
    /// The steps of colour arithmetic this thread has taken.
    static TAKEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Counts `n` steps of colour arithmetic.
fn take(n: u64) {
    TAKEN.with(|t| t.set(t.get().saturating_add(n)));
}

/// The steps of colour arithmetic this thread has taken: what an OSC's
/// allowance is reckoned from.
pub(super) fn taken() -> u64 {
    TAKEN.with(std::cell::Cell::get)
}

/// One step of a channel's intensity table: a device value and the
/// intensity it gives, as Xcms's `IntensityRec`.
type Step = (u16, f64);

/// The default screen's red intensities (`Default_RGB_RedTuples`).
const RED: [Step; 52] = [
    (0x0000, 0.000000),
    (0x0909, 0.000000),
    (0x0a0a, 0.000936),
    (0x0f0f, 0.001481),
    (0x1414, 0.002329),
    (0x1919, 0.003529),
    (0x1e1e, 0.005127),
    (0x2323, 0.007169),
    (0x2828, 0.009699),
    (0x2d2d, 0.012759),
    (0x3232, 0.016392),
    (0x3737, 0.020637),
    (0x3c3c, 0.025533),
    (0x4141, 0.031119),
    (0x4646, 0.037431),
    (0x4b4b, 0.044504),
    (0x5050, 0.052373),
    (0x5555, 0.061069),
    (0x5a5a, 0.070624),
    (0x5f5f, 0.081070),
    (0x6464, 0.092433),
    (0x6969, 0.104744),
    (0x6e6e, 0.118026),
    (0x7373, 0.132307),
    (0x7878, 0.147610),
    (0x7d7d, 0.163958),
    (0x8282, 0.181371),
    (0x8787, 0.199871),
    (0x8c8c, 0.219475),
    (0x9191, 0.240202),
    (0x9696, 0.262069),
    (0x9b9b, 0.285089),
    (0xa0a0, 0.309278),
    (0xa5a5, 0.334647),
    (0xaaaa, 0.361208),
    (0xafaf, 0.388971),
    (0xb4b4, 0.417945),
    (0xb9b9, 0.448138),
    (0xbebe, 0.479555),
    (0xc3c3, 0.512202),
    (0xc8c8, 0.546082),
    (0xcdcd, 0.581199),
    (0xd2d2, 0.617552),
    (0xd7d7, 0.655144),
    (0xdcdc, 0.693971),
    (0xe1e1, 0.734031),
    (0xe6e6, 0.775322),
    (0xebeb, 0.817837),
    (0xf0f0, 0.861571),
    (0xf5f5, 0.906515),
    (0xfafa, 0.952662),
    (0xffff, 1.000000),
];

/// The default screen's green intensities (`Default_RGB_GreenTuples`).
const GREEN: [Step; 50] = [
    (0x0000, 0.000000),
    (0x1313, 0.000000),
    (0x1414, 0.000832),
    (0x1919, 0.001998),
    (0x1e1e, 0.003612),
    (0x2323, 0.005736),
    (0x2828, 0.008428),
    (0x2d2d, 0.011745),
    (0x3232, 0.015740),
    (0x3737, 0.020463),
    (0x3c3c, 0.025960),
    (0x4141, 0.032275),
    (0x4646, 0.039449),
    (0x4b4b, 0.047519),
    (0x5050, 0.056520),
    (0x5555, 0.066484),
    (0x5a5a, 0.077439),
    (0x5f5f, 0.089409),
    (0x6464, 0.102418),
    (0x6969, 0.116485),
    (0x6e6e, 0.131625),
    (0x7373, 0.147853),
    (0x7878, 0.165176),
    (0x7d7d, 0.183604),
    (0x8282, 0.203140),
    (0x8787, 0.223783),
    (0x8c8c, 0.245533),
    (0x9191, 0.268384),
    (0x9696, 0.292327),
    (0x9b9b, 0.317351),
    (0xa0a0, 0.343441),
    (0xa5a5, 0.370580),
    (0xaaaa, 0.398747),
    (0xafaf, 0.427919),
    (0xb4b4, 0.458068),
    (0xb9b9, 0.489165),
    (0xbebe, 0.521176),
    (0xc3c3, 0.554067),
    (0xc8c8, 0.587797),
    (0xcdcd, 0.622324),
    (0xd2d2, 0.657604),
    (0xd7d7, 0.693588),
    (0xdcdc, 0.730225),
    (0xe1e1, 0.767459),
    (0xe6e6, 0.805235),
    (0xebeb, 0.843491),
    (0xf0f0, 0.882164),
    (0xf5f5, 0.921187),
    (0xfafa, 0.960490),
    (0xffff, 1.000000),
];

/// The default screen's blue intensities (`Default_RGB_BlueTuples`).
const BLUE: [Step; 51] = [
    (0x0000, 0.000000),
    (0x0e0e, 0.000000),
    (0x0f0f, 0.001341),
    (0x1414, 0.002080),
    (0x1919, 0.003188),
    (0x1e1e, 0.004729),
    (0x2323, 0.006766),
    (0x2828, 0.009357),
    (0x2d2d, 0.012559),
    (0x3232, 0.016424),
    (0x3737, 0.021004),
    (0x3c3c, 0.026344),
    (0x4141, 0.032489),
    (0x4646, 0.039481),
    (0x4b4b, 0.047357),
    (0x5050, 0.056154),
    (0x5555, 0.065903),
    (0x5a5a, 0.076634),
    (0x5f5f, 0.088373),
    (0x6464, 0.101145),
    (0x6969, 0.114968),
    (0x6e6e, 0.129862),
    (0x7373, 0.145841),
    (0x7878, 0.162915),
    (0x7d7d, 0.181095),
    (0x8282, 0.200386),
    (0x8787, 0.220791),
    (0x8c8c, 0.242309),
    (0x9191, 0.264937),
    (0x9696, 0.288670),
    (0x9b9b, 0.313499),
    (0xa0a0, 0.339410),
    (0xa5a5, 0.366390),
    (0xaaaa, 0.394421),
    (0xafaf, 0.423481),
    (0xb4b4, 0.453547),
    (0xb9b9, 0.484592),
    (0xbebe, 0.516587),
    (0xc3c3, 0.549498),
    (0xc8c8, 0.583291),
    (0xcdcd, 0.617925),
    (0xd2d2, 0.653361),
    (0xd7d7, 0.689553),
    (0xdcdc, 0.726454),
    (0xe1e1, 0.764013),
    (0xe6e6, 0.802178),
    (0xebeb, 0.840891),
    (0xf0f0, 0.880093),
    (0xf5f5, 0.919723),
    (0xfafa, 0.959715),
    (0xffff, 1.000000),
];

/// CIE XYZ to linear RGB intensity (`XYZtoRGBmatrix`).
const XYZ_TO_RGB: [[f64; 3]; 3] = [
    [
        3.483_404_812_535_39,
        -1.521_763_749_272_852,
        -0.559_231_333_540_497_8,
    ],
    [
        -1.071_527_513_061_936,
        1.965_937_952_043_724,
        0.03673691339553462,
    ],
    [
        0.06351179790497788,
        -0.200_205_010_004_964_8,
        0.810_709_420_316_482_2,
    ],
];

/// Linear RGB intensity to CIE XYZ (`RGBtoXYZmatrix`).
const RGB_TO_XYZ: [[f64; 3]; 3] = [
    [
        0.381_061_491_087_147_9,
        0.320_257_123_653_521_1,
        0.248_345_785_259_331,
    ],
    [
        0.207_297_451_151_408_5,
        0.680_546_387_763_732_4,
        0.112_156_161_084_859_2,
    ],
    [
        0.02133944350088028,
        0.142_971_930_202_464_8,
        1.241_728_926_296_655,
    ],
];

/// C's `DBL_EPSILON`, Xcms's `XMY_DBL_EPSILON`.
const DBL_EPSILON: f64 = f64::EPSILON;
/// How far outside 0 to 1 an intensity may be and still count as in the
/// gamut (`LRGB.c`'s `EPS`).
const GAMUT_SLACK: f64 = 0.001;
/// TekHVC's tolerance (`HVC.c`'s and `HVCMxC.c`'s `EPS`).
const HVC_SLACK: f64 = 0.001;
/// TekHVC's chroma per unit of u'v' distance and value.
const CHROMA_SCALE: f64 = 7.50725;

/// A colour's three coordinates, in whichever space it is in.
type Colour = [f64; 3];

/// `spec`, lowered, if it is one of the spaces this module reads: its
/// device colour, 16 bits a channel (each the 8 bits the screen keeps, in
/// the high byte); not read if the chroma search finds [`taken`] at
/// `until`. The steps it takes are spent from the call's budget.
pub(super) fn parse(spec: &[u8], until: u64) -> Option<[u16; 3]> {
    let before = taken();
    let colour = read(spec, until);
    spend!(colours, taken().saturating_sub(before));
    colour
}

/// [`parse`]'s reading.
fn read(spec: &[u8], until: u64) -> Option<[u16; 3]> {
    let colon = spec.iter().position(|b| *b == b':')?;
    let (prefix, rest) = spec.split_at_checked(colon)?;
    let numbers = three(rest.get(1..)?)?;
    let space = |name: &[u8]| prefix.eq_ignore_ascii_case(name);
    if space(b"rgbi") {
        if numbers.iter().any(|n| *n > 1.0) {
            return None;
        }
        return device(numbers.map(|n| n.max(0.0)));
    }
    let xyz = if space(b"ciexyz") {
        valid_xyz(numbers)?
    } else if space(b"cieuvy") {
        uvy_to_xyz(numbers)?
    } else if space(b"ciexyy") {
        xyy_to_xyz(numbers)?
    } else if space(b"cielab") {
        lab_to_xyz(numbers)?
    } else if space(b"cieluv") {
        uvy_to_xyz(luv_to_uvy(numbers)?)?
    } else if space(b"tekhvc") {
        uvy_to_xyz(hvc_to_uvy(numbers)?)?
    } else {
        return None;
    };
    device(intensities(xyz, until)?)
}

/// `%lf/%lf/%lf`, as `sscanf` reads it: three numbers, each after any
/// white space, with a `/` straight after each of the first two; what
/// follows the third is left. If that fails, it is read again with each
/// `.` and `,` swapped, as each Xcms `ParseString` retries.
fn three(text: &[u8]) -> Option<Colour> {
    three_once(text).or_else(|| {
        let swapped: Vec<u8> = (text.iter())
            .map(|b| match b {
                b'.' => b',',
                b',' => b'.',
                b => *b,
            })
            .collect();
        three_once(&swapped)
    })
}

/// [`three`]'s reading, once.
fn three_once(text: &[u8]) -> Option<Colour> {
    let (a, text) = number(text)?;
    let (b, text) = number(text.strip_prefix(b"/")?)?;
    let (c, _) = number(text.strip_prefix(b"/")?)?;
    Some([a, b, c])
}

/// A decimal number after any white space, as `%lf` reads one: a sign,
/// digits with a point among them, and an exponent; and what follows it.
fn number(text: &[u8]) -> Option<(f64, &[u8])> {
    let start = text
        .iter()
        .position(|b| !b" \t\n\x0b\x0c\r".contains(b))
        .unwrap_or(text.len());
    let text = text.get(start..)?;
    let digits = |from: usize| {
        text.get(from..)
            .map_or(0, |t| t.iter().take_while(|b| b.is_ascii_digit()).count())
    };
    let mut end = usize::from(matches!(text.first(), Some(b'+' | b'-')));
    let whole = digits(end);
    end = end.saturating_add(whole);
    let mut fraction = 0;
    if text.get(end) == Some(&b'.') {
        fraction = digits(end.saturating_add(1));
        end = end.saturating_add(1).saturating_add(fraction);
    }
    if whole == 0 && fraction == 0 {
        return None;
    }
    if matches!(text.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(text.get(end.saturating_add(1)), Some(b'+' | b'-')));
        let power = digits(end.saturating_add(1).saturating_add(sign));
        if power > 0 {
            end = end
                .saturating_add(1)
                .saturating_add(sign)
                .saturating_add(power);
        }
    }
    let (number, rest) = text.split_at_checked(end)?;
    let value: f64 = str::from_utf8(number).ok()?.parse().ok()?;
    value.is_finite().then_some((value, rest))
}

/// The screen's white point in CIE XYZ: the RGB matrix's row sums, Y made
/// 1 (`_XcmsLRGB_InitScrnDefault`).
fn white() -> Colour {
    let sum = |row: [f64; 3]| row.iter().fold(0.0, |s, x| s + x);
    let [x, _, z] = RGB_TO_XYZ;
    [sum(x), 1.0, sum(z)]
}

/// The white point's u' and v'.
fn white_uv() -> (f64, f64) {
    let [x, y, z] = white();
    let div = x + 15.0 * y + 3.0 * z;
    (4.0 * x / div, 9.0 * y / div)
}

/// `m` times `v`, summed left to right as `_XcmsMatVec` sums.
fn times(m: &[[f64; 3]; 3], v: Colour) -> Colour {
    m.map(|row| row.iter().zip(v).fold(0.0, |s, (a, b)| s + a * b))
}

/// Xcms's checks of a value against a range, with its slack.
fn within(v: f64, low: f64, high: f64) -> bool {
    v >= low - DBL_EPSILON && v <= high + DBL_EPSILON
}

/// A CIE XYZ colour if its Y is 0 to 1 (`_XcmsCIEXYZ_ValidSpec`).
fn valid_xyz(c: Colour) -> Option<Colour> {
    let [_, y, _] = c;
    within(y, 0.0, 1.0).then_some(c)
}

/// CIE XYZ to CIE u'v'Y (`XcmsCIEXYZToCIEuvY`).
fn xyz_to_uvy(c: Colour) -> Option<Colour> {
    let [x, y, z] = valid_xyz(c)?;
    let div = x + 15.0 * y + 3.0 * z;
    if div == 0.0 {
        let (u, v) = white_uv();
        return Some([u, v, y]);
    }
    Some([4.0 * x / div, 9.0 * y / div, y])
}

/// CIE u'v'Y to CIE XYZ (`XcmsCIEuvYToCIEXYZ`).
fn uvy_to_xyz([u, v, luminance]: Colour) -> Option<Colour> {
    if !within(luminance, 0.0, 1.0) {
        return None;
    }
    let mut div = 6.0 * u - 16.0 * v + 12.0;
    let (mut u, mut v) = (u, v);
    if div == 0.0 {
        (u, v) = white_uv();
        div = 6.0 * u - 16.0 * v + 12.0;
    }
    let x = 9.0 * u / div;
    let y = 4.0 * v / div;
    let z = 1.0 - x - y;
    if y == 0.0 {
        return Some([x, luminance, z]);
    }
    Some([x * luminance / y, luminance, z * luminance / y])
}

/// CIE xyY to CIE XYZ (`XcmsCIExyYToCIEXYZ`).
fn xyy_to_xyz(c: Colour) -> Option<Colour> {
    if !c.iter().all(|v| within(*v, 0.0, 1.0)) {
        return None;
    }
    let [x, y, luminance] = c;
    let div = -2.0 * x + 12.0 * y + 3.0;
    if div == 0.0 {
        return Some([0.0; 3]);
    }
    let (u, v) = (4.0 * x / div, 9.0 * y / div);
    let div = 6.0 * u - 16.0 * v + 12.0;
    if div == 0.0 {
        // Xcms reads a white point it never filled in here.
        return None;
    }
    let x = 9.0 * u / div;
    let mut y = 4.0 * v / div;
    let z = 1.0 - x - y;
    if y == 0.0 {
        y = 0.00001;
    }
    Some([x * luminance / y, luminance, z * luminance / y])
}

/// CIE L*a*b* to CIE XYZ (`XcmsCIELabToCIEXYZ`), as Xcms reckons it.
fn lab_to_xyz([l, a, b]: Colour) -> Option<Colour> {
    if !within(l, 0.0, 100.0) {
        return None;
    }
    let [wx, _, wz] = white();
    let t = (l + 16.0) / 116.0;
    let y = t * t * t;
    if y < 0.008856 {
        let t = l / 9.03292;
        return Some([wx * ((a / 3893.5) + t), t, wz * (t - (b / 1557.4))]);
    }
    let cube = |v: f64| v * v * v;
    Some([wx * cube(t + a / 5.0), y, wz * cube(t - b / 2.0)])
}

/// The luminance of a CIE L* or a TekHVC value.
fn luminance(lightness: f64) -> f64 {
    if lightness < 7.99953624 {
        return lightness / 903.29;
    }
    let t = (lightness + 16.0) / 116.0;
    t * t * t
}

/// CIE L*u*v* to CIE u'v'Y (`XcmsCIELuvToCIEuvY`).
fn luv_to_uvy([l, u, v]: Colour) -> Option<Colour> {
    if !within(l, 0.0, 100.0) {
        return None;
    }
    let (wu, wv) = white_uv();
    if l == 0.0 {
        return Some([wu, wv, luminance(l)]);
    }
    let t = 13.0 * (l / 100.0);
    Some([u / t + wu, v / t + wv, luminance(l)])
}

/// The hue of TekHVC's best red from the white point, in degrees, as
/// Xcms reckons it each time (`ThetaOffset`):
/// `degrees(atan((0.4931 - wv) / (0.7127 - wu)))`, from TekHVC's
/// best red (`u_BR`, `v_BR`) and the white point (a test checks it).
const THETA_OFFSET: f64 = 5.105096984039076;

/// A TekHVC colour brought into its ranges, if it is in them give or take
/// Xcms's slack (`XcmsTekHVC_ValidSpec`).
fn valid_hvc([h, v, c]: Colour) -> Option<Colour> {
    if !within(v, 0.0, 100.0) || c < -DBL_EPSILON {
        return None;
    }
    Some(modify([h, v, c]))
}

/// TekHVC to CIE u'v'Y (`XcmsTekHVCToCIEuvY`).
fn hvc_to_uvy(c: Colour) -> Option<Colour> {
    let [h, v, c] = valid_hvc(c)?;
    let (wu, wv) = white_uv();
    if v == 0.0 || v == 100.0 {
        return Some([wu, wv, if v == 100.0 { 1.0 } else { 0.0 }]);
    }
    let hue = turn(h + THETA_OFFSET, 0.0, 360.0) * PI / 180.0;
    let u = (cos(hue) * c) / (v * CHROMA_SCALE);
    let w = (sin(hue) * c) / (v * CHROMA_SCALE);
    Some([u + wu, w + wv, luminance(v)])
}

/// Radians in degrees, as Xcms's `degrees` reckons them.
fn degrees(radians: f64) -> f64 {
    radians * 180.0 / PI
}

/// `degrees` turned by whole turns into `low` to `high`, as Xcms's loops
/// turn a hue. A hue many turns out is first brought within one by the
/// remainder, which is exact, as each turn the loop takes is: the loop
/// itself then takes at most two.
fn turn(degrees: f64, low: f64, high: f64) -> f64 {
    let mut degrees = if degrees.abs() > 720.0 {
        degrees % 360.0
    } else {
        degrees
    };
    for _ in 0..4 {
        if degrees < low {
            degrees += 360.0;
        } else if degrees >= high {
            degrees -= 360.0;
        } else {
            break;
        }
    }
    degrees
}

/// CIE u'v'Y to TekHVC (`XcmsCIEuvYToTekHVC`).
fn uvy_to_hvc([u, v, y]: Colour) -> Option<Colour> {
    if !within(y, 0.0, 1.0) {
        return None;
    }
    let (wu, wv) = white_uv();
    let (u, v) = (u - wu, v - wv);
    let mut theta = if u == 0.0 { 0.0 } else { degrees(atan(v / u)) };
    let (low, high) = match (u > 0.0, u < 0.0, v > 0.0, v < 0.0) {
        (true, _, true, _) => (0.0, 90.0),
        (_, true, true, _) => (90.0, 180.0),
        (_, true, _, true) => (180.0, 270.0),
        (true, _, _, true) => (270.0, 360.0),
        _ => (0.0, 360.0),
    };
    for _ in 0..8 {
        if theta < low {
            theta += 90.0;
        } else if theta >= high {
            theta -= 90.0;
        } else {
            break;
        }
    }
    let lightness = if y < 0.008856 {
        y * 903.29
    } else {
        cube_root(y) * 116.0 - 16.0
    };
    let chroma = lightness * CHROMA_SCALE * square_root(u * u + v * v);
    if chroma < 0.0 {
        theta = 0.0;
    }
    let hue = turn(theta - THETA_OFFSET, -HVC_SLACK, 360.0 + HVC_SLACK);
    Some([hue, lightness, chroma])
}

/// A TekHVC colour clamped into its ranges (`_XcmsTekHVC_CheckModify`).
fn modify([h, v, c]: Colour) -> Colour {
    let v = if v < 0.0 {
        DBL_EPSILON
    } else if v > 100.0 {
        100.0 - DBL_EPSILON
    } else {
        v
    };
    let c = if c < 0.0 { -DBL_EPSILON } else { c };
    [turn(h, 0.0, 360.0), v, c]
}

/// The linear intensities of CIE XYZ, unclamped.
fn raw(xyz: Colour) -> Colour {
    times(&XYZ_TO_RGB, xyz)
}

/// Whether intensities are outside the screen's gamut.
fn outside([r, g, b]: Colour) -> bool {
    r.min(g).min(b) < -GAMUT_SLACK || r.max(g).max(b) > 1.0 + GAMUT_SLACK
}

/// The screen's intensities for CIE XYZ, the TekHVC chroma lowered first
/// if it is out of the gamut (`XcmsCIEXYZToRGBi`, `XcmsTekHVCClipC`).
fn intensities(xyz: Colour, until: u64) -> Option<Colour> {
    let mut rgb = raw(xyz);
    if !rgb.iter().all(|i| i.is_finite()) {
        return None;
    }
    if outside(rgb) {
        let [h, v, _] = uvy_to_hvc(xyz_to_uvy(xyz)?)?;
        rgb = raw(uvy_to_xyz(hvc_to_uvy(max_chroma(h, v, until)?)?)?);
        if outside(rgb) {
            return None;
        }
    }
    Some(rgb.map(|i| i.clamp(0.0, 1.0)))
}

/// TekHVC of linear intensities.
fn rgb_to_hvc(rgb: Colour) -> Option<Colour> {
    uvy_to_hvc(xyz_to_uvy(times(&RGB_TO_XYZ, rgb))?)
}

/// The most chromatic colour of `hue` on the gamut's edge, and its
/// intensities (`_XcmsTekHVCQueryMaxVCRGB`).
fn most_chromatic(hue: f64) -> Option<(Colour, Colour)> {
    let mut rgb = raw(uvy_to_xyz(hvc_to_uvy([hue, 40.0, 120.0])?)?);
    if !outside(rgb) {
        rgb = rgb.map(|i| i.clamp(0.0, 1.0));
    }
    let [r, g, b] = rgb;
    let small = r.min(g.min(b));
    let rgb = rgb.map(|i| i - small);
    let [r, g, b] = rgb;
    let large = r.max(g.max(b));
    let rgb = rgb.map(|i| i / large);
    let [_, v, c] = rgb_to_hvc(rgb)?;
    Some(([hue, v, c], rgb))
}

/// The most chroma `hue` has at `value` in the gamut
/// (`XcmsTekHVCQueryMaxC`); none once [`taken`] reaches `until`.
fn max_chroma(hue: f64, value: f64, until: u64) -> Option<Colour> {
    let start = modify([hue, value, 100.0]);
    let ([_, top_v, top_c], top) = most_chromatic(hue)?;
    if value <= top_v {
        let [h, v, _] = start;
        return Some(modify([h, v, value * top_c / top_v]));
    }
    let [_, mut v, mut c] = start;
    let mut target = value;
    let (mut last_v, mut last_c) = (-1.0, -1.0);
    let distance = 100.0 - top_v;
    let mut factor = 1.0;
    for _ in 0..100 {
        take(1);
        if taken() >= until {
            return None;
        }
        let before = last_v;
        (last_v, last_c) = (v, c);
        let t = (target - top_v) / distance * factor;
        [_, v, c] = rgb_to_hvc(top.map(|i| i * (1.0 - t) + t))?;
        if v <= value + HVC_SLACK && v >= value - HVC_SLACK {
            return Some(modify([hue, v, c]));
        }
        target += value - v;
        if target < top_v {
            target = top_v;
            factor *= 0.5;
        } else if target > 100.0 {
            if (last_v - value).abs() < (v - value).abs() {
                (v, c) = (last_v, last_c);
            }
            return Some(modify([hue, v, c]));
        } else if v <= before + HVC_SLACK && v >= before - HVC_SLACK {
            factor *= 0.5;
        }
    }
    if (last_v - value).abs() < (v - value).abs() {
        (v, c) = (last_v, last_c);
    }
    Some([hue, v, c])
}

/// The device colour of linear intensities 0 to 1, each channel's
/// intensity looked up in its table (`XcmsRGBiToRGB`).
fn device([r, g, b]: Colour) -> Option<[u16; 3]> {
    Some([level(&RED, r)?, level(&GREEN, g)?, level(&BLUE, b)?])
}

/// The device value of `intensity` in `table`, at 8 bits
/// (`_XcmsTableSearch`, `_XcmsIntensityInterpolation`).
fn level(table: &[Step], intensity: f64) -> Option<u16> {
    let (mut lo, mut hi) = (0usize, table.len().checked_sub(1)?);
    let (first, at_first) = *table.first()?;
    if intensity <= at_first {
        return Some(first & 0xff00);
    }
    let (mut last, mut mid) = (hi, lo);
    while mid != last {
        last = mid;
        mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        let (value, at) = *table.get(mid)?;
        if intensity == at {
            return Some(value & 0xff00);
        } else if intensity < at {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let ((low, at_low), (high, at_high)) = (*table.get(lo)?, *table.get(hi)?);
    let ratio = (intensity - at_low) / (at_high - at_low);
    let target = u32::from(truncated(f64::from(high.saturating_sub(low)) * ratio))
        .saturating_add(u32::from(low));
    // The values the screen holds are the multiples of 0x101; the nearer
    // of the two either side of `target`, the lower on a tie.
    let step = |n: u32| n.min(255).saturating_mul(0x101);
    let mut up = step(target >> 8);
    let down;
    if up < target {
        down = up;
        up = step((down >> 8).saturating_add(1));
    } else {
        down = step((up >> 8).saturating_sub(1));
    }
    let nearer = if up.saturating_sub(target) < target.saturating_sub(down) {
        up
    } else {
        down
    };
    u16::try_from(nearer & 0xff00).ok()
}

/// `x`, 0 to 65535, with its fraction dropped, as C converts a double to
/// an integer.
fn truncated(x: f64) -> u16 {
    (0..16u32).rev().fold(0u16, |n, bit| {
        let more = n | 1u16.checked_shl(bit).unwrap_or(0);
        if f64::from(more) <= x { more } else { n }
    })
}

/// Xcms's square root: Newton's method from a quarter, or four times
/// (`_XcmsSquareRoot`).
fn square_root(a: f64) -> f64 {
    if a <= 0.0 {
        return 0.0;
    }
    let mut guess = if a > 1.0 { a / 4.0 } else { a * 4.0 };
    let mut steps = 0u64;
    while steps < 600 {
        steps = steps.saturating_add(1);
        let delta = (guess - a / guess) / 2.0;
        guess -= delta;
        if delta.abs() < guess * DBL_EPSILON || delta.is_nan() {
            break;
        }
    }
    take(steps);
    guess
}

/// Xcms's cube root: Newton's method from an eighth, or eight times
/// (`_XcmsCubeRoot`).
fn cube_root(a: f64) -> f64 {
    if a == 0.0 {
        return 0.0;
    }
    let magnitude = a.abs();
    let mut guess = if magnitude > 1.0 {
        magnitude / 8.0
    } else {
        magnitude * 8.0
    };
    let mut steps = 0u64;
    while steps < 64 {
        steps = steps.saturating_add(1);
        let delta = (guess - magnitude / (guess * guess)) / 3.0;
        guess -= delta;
        if delta.abs() < guess * DBL_EPSILON || delta.is_nan() {
            break;
        }
    }
    take(steps);
    if a < 0.0 { -guess } else { guess }
}

/// Below this, x⁶ underflows and the series is x itself.
const X6_UNDERFLOWS: f64 = 4.209340e-52;

/// The rational approximations' coefficients, lowest first.
const COS_P: [f64; 4] = [
    1.290_539_465_903_737_3e6,
    -3.745_670_391_572_320_4e5,
    1.343_230_098_653_908_5e4,
    -1.123_145_082_334_093_3e2,
];
/// See [`COS_P`].
const COS_Q: [f64; 4] = [
    1.290_539_465_903_737_3e6,
    2.346_777_310_724_583_4e4,
    2.096_951_819_672_630_7e2,
    1.0,
];
/// See [`COS_P`].
const SIN_P: [f64; 4] = [
    2.066_434_333_699_585_9e6,
    -1.816_039_879_740_733_3e5,
    3.599_930_694_963_619e3,
    -2.010_748_329_458_861_5e1,
];
/// See [`COS_P`].
const SIN_Q: [f64; 4] = [
    2.631_065_910_264_77e6,
    3.927_024_277_464_9e4,
    2.781_191_948_108_384_5e2,
    1.0,
];

/// The polynomial of `coefficients` at `x`, by Horner's rule
/// (`_XcmsPolynomial`).
fn polynomial(coefficients: &[f64; 4], x: f64) -> f64 {
    coefficients.iter().rev().fold(0.0, |sum, c| c + x * sum)
}

/// `x` brought into -π to π (`_XcmsModulo` and its callers).
fn reduce(x: f64) -> f64 {
    if (-PI..=PI).contains(&x) {
        return x;
    }
    // _XcmsModuloF: the fraction, by adding and taking away the largest
    // power of two a double holds exactly.
    const BIG: f64 = 2_147_483_647.0 * 4_194_304.0;
    let turns = x / TWO_PI;
    let whole = if turns.abs() >= BIG {
        turns
    } else {
        let a = turns.abs();
        let i = a + BIG - BIG;
        (if i > a { i - 1.0 } else { i }).abs()
    };
    let x = (turns - whole) * TWO_PI;
    if x > PI {
        x - TWO_PI
    } else if x < -PI {
        x + TWO_PI
    } else {
        x
    }
}

/// Xcms's cosine (`_XcmsCosine`).
fn cos(x: f64) -> f64 {
    let x = reduce(x);
    if x > HALF_PI {
        -cos(x - PI)
    } else if x < -HALF_PI {
        -cos(x + PI)
    } else if x > FOURTH_PI {
        sin(HALF_PI - x)
    } else if x < -FOURTH_PI {
        sin(HALF_PI + x)
    } else if x < X6_UNDERFLOWS && x > -X6_UNDERFLOWS {
        square_root(1.0 - x * x)
    } else {
        let y = x / FOURTH_PI;
        polynomial(&COS_P, y * y) / polynomial(&COS_Q, y * y)
    }
}

/// Xcms's sine (`_XcmsSine`).
fn sin(x: f64) -> f64 {
    let x = reduce(x);
    if x > HALF_PI {
        -sin(x - PI)
    } else if x < -HALF_PI {
        -sin(x + PI)
    } else if x > FOURTH_PI {
        cos(HALF_PI - x)
    } else if x < -FOURTH_PI {
        -cos(HALF_PI + x)
    } else if x < X6_UNDERFLOWS && x > -X6_UNDERFLOWS {
        x
    } else {
        let y = x / FOURTH_PI;
        y * (polynomial(&SIN_P, y * y) / polynomial(&SIN_Q, y * y))
    }
}

/// Xcms's arc tangent, by the arithmetic-geometric mean
/// (`_XcmsArcTangent`).
fn atan(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let most = if x < 1.0 { x * 0.000001 } else { 0.000001 };
    let (mut a, mut b) = (square_root(1.0 / (1.0 + x * x)), 1.0);
    let (mut a1, mut b1) = (0.0, 0.0);
    let mut steps = 0u64;
    while steps < 64 {
        steps = steps.saturating_add(1);
        a1 = (a + b) / 2.0;
        b1 = square_root(a1 * b);
        if a1 == b1 || (a1 - b1).abs() < most {
            break;
        }
        (a, b) = (a1, b1);
    }
    take(steps);
    let l = if a1 > b1 { b1 } else { a1 };
    x / (square_root(1.0 + x * x) * l)
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// The best red's hue is the one Xcms works out from the white point.
    #[test]
    fn the_theta_offset_is_xcms_own() {
        let (wu, wv) = super::white_uv();
        let offset = super::degrees(super::atan((0.4931 - wv) / (0.7127 - wu)));
        assert_eq!(offset.to_bits(), super::THETA_OFFSET.to_bits());
    }

    /// What Xcms refuses, and what this reading refuses beyond it.
    #[test]
    fn what_cannot_be_read_changes_nothing() {
        for bad in [
            "rgbi:1/1",
            "rgbi:2/0/0",
            "CIEXYZ:0/2/0",
            "CIExyY:2/0/0",
            "CIELab:101/0/0",
            "CIELuv:-1/0/0",
            "TekHVC:0/101/0",
            "TekHVC:0/50/-1",
            "CIEXYZ:inf/0/0",
            "CIEXYZ:nan/0/0",
            "CIEXYZ:1e999/0/0",
            "CIEXYZ:./0/0",
            "CIEXYZ 0/0/0",
            "lab:0/0/0",
        ] {
            assert_eq!(parse(bad.as_bytes(), u64::MAX), None, "{bad}");
        }
        // As %lf reads them: white space before a number, an exponent, and
        // anything after the third.
        assert_eq!(
            parse(b"rgbi: 1/ 1e0/+1.junk", u64::MAX),
            parse(b"rgbi:1/1/1", u64::MAX)
        );
    }

    /// A chroma far past the gamut still ends inside it, in a bounded
    /// number of steps.
    #[test]
    fn far_outside_the_gamut_is_brought_in() {
        for spec in [
            "TekHVC:0/50/1e9",
            "TekHVC:200/99/1e300",
            "CIEuvY:1e300/-1e300/0.5",
        ] {
            assert!(parse(spec.as_bytes(), u64::MAX).is_some(), "{spec}");
        }
    }
}
