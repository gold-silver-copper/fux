"""Statistics for the comparisons: Wilson intervals, Fisher's and McNemar's exact tests,
Wilcoxon signed-rank (normal approximation with tie and continuity
corrections, exact below 20 pairs), Cochran's Q, Friedman and Holm's correction."""
import itertools, math


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 0.0)
    p = k / n; d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d; h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (100 * (c - h), 100 * (c + h))


def fisher(a, b, c, d):
    n = a + b + c + d; r1 = a + b; c1 = a + c
    def p(x):
        return math.exp(math.lgamma(r1 + 1) + math.lgamma(n - r1 + 1) + math.lgamma(c1 + 1) + math.lgamma(n - c1 + 1)
                        - math.lgamma(n + 1) - math.lgamma(x + 1) - math.lgamma(r1 - x + 1)
                        - math.lgamma(c1 - x + 1) - math.lgamma(n - r1 - c1 + x + 1))
    lo, hi = max(0, r1 + c1 - n), min(r1, c1)
    pa = p(a)
    return min(1.0, sum(p(x) for x in range(lo, hi + 1) if p(x) <= pa * (1 + 1e-9)))


def mcnemar(b, c):
    """Exact two-sided McNemar on discordant pairs b (only first right), c (only second right)."""
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    return min(1.0, 2 * sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n)


def wilcoxon(xs, ys):
    """Two-sided signed-rank test of paired samples; returns (p, median difference y - x, n used)."""
    # Differences of means of repeats (thirds and the like) are rounded, so
    # equal magnitudes tie, as they do in exact arithmetic.
    d = [round(y - x, 9) for x, y in zip(xs, ys) if round(y - x, 9) != 0]
    n = len(d)
    diffs = sorted(y - x for x, y in zip(xs, ys))
    med = diffs[len(diffs) // 2] if diffs else 0.0
    if n == 0:
        return 1.0, med, 0
    order = sorted(range(n), key=lambda i: abs(d[i]))
    ranks = [0.0] * n
    i = 0
    while i < n:
        j = i
        while j + 1 < n and abs(d[order[j + 1]]) == abs(d[order[i]]):
            j += 1
        for k in range(i, j + 1):
            ranks[order[k]] = (i + j) / 2 + 1
        i = j + 1
    wplus = sum(r for r, v in zip(ranks, d) if v > 0)
    if n < 20:
        total = 0; extreme = 0
        target = min(wplus, n * (n + 1) / 2 - wplus)
        for signs in itertools.product((0, 1), repeat=n):
            w = sum(r for r, s in zip(ranks, signs) if s)
            total += 1
            if min(w, n * (n + 1) / 2 - w) <= target + 1e-9:
                extreme += 1
        return extreme / total, med, n
    mean = n * (n + 1) / 4
    ties = {}
    for r in ranks:
        ties[r] = ties.get(r, 0) + 1
    var = n * (n + 1) * (2 * n + 1) / 24 - sum(t ** 3 - t for t in ties.values()) / 48
    z = (abs(wplus - mean) - 0.5) / math.sqrt(var) if var > 0 else 0.0
    return math.erfc(z / math.sqrt(2)), med, n


def chi2_sf(x, k):
    """Upper tail of the chi-squared distribution with k degrees of freedom
    (the regularized upper incomplete gamma Q(k/2, x/2))."""
    if x <= 0:
        return 1.0
    a, z = k / 2, x / 2
    if z < a + 1:  # series for the lower part
        term = s = 1 / a
        n = a
        while abs(term) > 1e-15 * abs(s):
            n += 1
            term *= z / n
            s += term
        return max(0.0, 1 - s * math.exp(-z + a * math.log(z) - math.lgamma(a)))
    # continued fraction for the upper part (Lentz)
    tiny = 1e-300
    b = z + 1 - a; c = 1 / tiny; d = 1 / b; h = d
    for i in range(1, 10000):
        an = -i * (i - a); b += 2
        d = an * d + b; d = tiny if abs(d) < tiny else d
        c = b + an / c; c = tiny if abs(c) < tiny else c
        d = 1 / d; delta = d * c; h *= delta
        if abs(delta - 1) < 1e-15:
            break
    return min(1.0, math.exp(-z + a * math.log(z) - math.lgamma(a)) * h)


def cochran_q(rows):
    """Cochran's Q over blocks (tasks) of binary outcomes, one per tool:
    rows = [[0/1 per tool], ...]. Returns (Q, p)."""
    k = len(rows[0]) if rows else 0
    cols = [sum(r[j] for r in rows) for j in range(k)]
    tot = sum(cols)
    den = k * tot - sum(sum(r) ** 2 for r in rows)
    if k < 2 or den == 0:
        return 0.0, 1.0
    q = (k - 1) * (k * sum(c * c for c in cols) - tot * tot) / den
    return q, chi2_sf(q, k - 1)


def friedman(rows):
    """Friedman's test over blocks (tasks) of one measure per tool, with
    mid-ranks for ties. Returns (statistic, p)."""
    n = len(rows); k = len(rows[0]) if rows else 0
    if n == 0 or k < 2:
        return 0.0, 1.0
    rank_sums = [0.0] * k
    tie_term = 0.0
    for r in rows:
        order = sorted(range(k), key=lambda j: r[j])
        ranks = [0.0] * k
        i = 0
        while i < k:
            j = i
            while j + 1 < k and r[order[j + 1]] == r[order[i]]:
                j += 1
            for m in range(i, j + 1):
                ranks[order[m]] = (i + j) / 2 + 1
            tie_term += (j - i + 1) ** 3 - (j - i + 1)
            i = j + 1
        for j in range(k):
            rank_sums[j] += ranks[j]
    num = 12 * sum(s * s for s in rank_sums) - 3 * n * n * k * (k + 1) ** 2
    den = n * k * (k + 1) - tie_term / (k - 1)
    if den == 0:
        return 0.0, 1.0
    stat = num / den
    return stat, chi2_sf(stat, k - 1)


def holm(ps):
    """Holm-adjusted p-values, in the order given."""
    m = len(ps)
    order = sorted(range(m), key=lambda i: ps[i])
    out = [0.0] * m
    running = 0.0
    for rank, i in enumerate(order):
        running = max(running, min(1.0, (m - rank) * ps[i]))
        out[i] = running
    return out
