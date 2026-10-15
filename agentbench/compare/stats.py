"""bench-161 statistics: Wilson intervals, Fisher's and McNemar's exact tests,
Wilcoxon signed-rank (normal approximation with tie and continuity
corrections, exact below 20 pairs)."""
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
    d = [y - x for x, y in zip(xs, ys) if y != x]
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
