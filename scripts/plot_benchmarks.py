#!/usr/bin/env python3
"""Draw the README's upstream-comparison chart from BENCHMARKS.md.

The chart is generated, never hand-edited, so that it cannot drift from the
report it claims to summarise. It stamps the revision recorded in the report's
own settings table onto the image: a chart whose stamp is behind `git log` is
visibly stale rather than quietly wrong.

Only the per-case one-shot tables are read. Absolute compression ratios are
deliberately not plotted -- the corpora are generated fixtures, so their ratios
describe the generator rather than any real workload. What transfers is the
comparison against upstream on identical bytes, which is what both panels show.

Usage:
    python3 scripts/plot_benchmarks.py [--report BENCHMARKS.md]
                                       [--output assets/benchmarks.svg]
"""

from __future__ import annotations

import argparse
import math
import pathlib
import re
import statistics
import sys

# One row of a per-case table: level, ok, ok, then six numbers -- this crate's
# ratio and upstream's, this crate's encode MiB/s and upstream's, then the same
# for decode. Rows that did not complete are not `ok` and do not match.
ROW = re.compile(
    r"\|\s*(\d+)\s*\|\s*ok\s*\|\s*ok\s*\|" + r"\s*([\d.]+)\s*\|" * 6
)
SETTING = re.compile(r"^\|\s*(.+?)\s*\|\s*`?([^`|]+?)`?\s*\|$")

LEVELS = range(1, 23)


class ReportError(RuntimeError):
    """The report is missing something the chart cannot be drawn without."""


def parse_report(text: str) -> tuple[dict[str, dict[int, list[float]]], dict[str, str]]:
    """Return per-case rows keyed by level, and the settings table."""
    settings: dict[str, str] = {}
    for line in text.splitlines():
        match = SETTING.match(line)
        if match and match.group(1) not in ("---", "Setting"):
            settings.setdefault(match.group(1), match.group(2))

    cases: dict[str, dict[int, list[float]]] = {}
    for section in text.split("\n## ")[1:]:
        name = section.split("\n", 1)[0].strip()
        rows = {}
        for line in section.splitlines():
            match = ROW.match(line)
            if match:
                values = [float(v) for v in match.groups()]
                rows[int(values[0])] = values[1:]
        if rows:
            cases[name] = rows
    if not cases:
        raise ReportError("no per-case tables found; is this a benchmark report?")
    return cases, settings


def series(cases: dict[str, dict[int, list[float]]]) -> dict[str, dict[int, tuple[float, float, float]]]:
    """Per level, the (median, min, max) of each comparison across all cases.

    Size is a signed percentage against upstream, so 0 means byte-identical and
    a negative value means this crate emitted less. Throughput is a percentage
    *of* upstream, so 100 is parity and less is slower.
    """
    out: dict[str, dict[int, tuple[float, float, float]]] = {
        "size": {},
        "encode": {},
        "decode": {},
    }
    for level in LEVELS:
        rows = [c[level] for c in cases.values() if level in c]
        if not rows:
            raise ReportError(f"level {level} is missing from every case")
        for key, values in (
            ("size", [(r[0] / r[1] - 1.0) * 100.0 for r in rows]),
            ("encode", [r[2] / r[3] * 100.0 for r in rows]),
            ("decode", [r[4] / r[5] * 100.0 for r in rows]),
        ):
            out[key][level] = (statistics.median(values), min(values), max(values))
    return out


class Panel:
    """Maps data coordinates onto a rectangle of the drawing."""

    def __init__(self, x: float, y: float, w: float, h: float, lo: float, hi: float):
        self.x, self.y, self.w, self.h = x, y, w, h
        self.lo, self.hi = lo, hi

    def px(self, level: int) -> float:
        return self.x + self.w * (level - LEVELS.start) / (LEVELS.stop - 1 - LEVELS.start)

    def py(self, value: float) -> float:
        clamped = min(max(value, self.lo), self.hi)
        return self.y + self.h * (1.0 - (clamped - self.lo) / (self.hi - self.lo))

    def line(self, points: dict[int, float]) -> str:
        return " ".join(
            f"{'M' if i == 0 else 'L'}{self.px(lv):.1f},{self.py(v):.1f}"
            for i, (lv, v) in enumerate(sorted(points.items()))
        )

    def band(self, lows: dict[int, float], highs: dict[int, float]) -> str:
        top = " ".join(
            f"{'M' if i == 0 else 'L'}{self.px(lv):.1f},{self.py(v):.1f}"
            for i, (lv, v) in enumerate(sorted(highs.items()))
        )
        bottom = " ".join(
            f"L{self.px(lv):.1f},{self.py(v):.1f}" for lv, v in sorted(lows.items(), reverse=True)
        )
        return f"{top} {bottom} Z"


def axes(panel: Panel, ticks: list[float], fmt: str, title: str, subtitle: str) -> list[str]:
    # The two panels run in opposite directions -- fewer bytes is a win, fewer
    # MiB/s is not -- so each says which way is good rather than leaving the
    # reader to infer it from the sign.
    out = [
        f'<text x="{panel.x:.0f}" y="{panel.y - 40:.0f}" class="ptitle">{title}</text>',
        f'<text x="{panel.x:.0f}" y="{panel.y - 21:.0f}" class="psub">{subtitle}</text>',
    ]
    for tick in ticks:
        y = panel.py(tick)
        out.append(f'<line x1="{panel.x:.0f}" y1="{y:.1f}" x2="{panel.x + panel.w:.0f}" y2="{y:.1f}" class="grid"/>')
        out.append(f'<text x="{panel.x - 8:.0f}" y="{y + 4:.1f}" class="ytick">{fmt.format(tick)}</text>')
    out.append(
        f'<line x1="{panel.x:.0f}" y1="{panel.y + panel.h:.0f}" x2="{panel.x + panel.w:.0f}" y2="{panel.y + panel.h:.0f}" class="axis"/>'
    )
    for level in (1, 5, 10, 15, 22):
        out.append(
            f'<text x="{panel.px(level):.1f}" y="{panel.y + panel.h + 20:.0f}" class="xtick">{level}</text>'
        )
    out.append(
        f'<text x="{panel.x + panel.w / 2:.0f}" y="{panel.y + panel.h + 42:.0f}" class="axlabel">compression level</text>'
    )
    return out


def render(data: dict[str, dict[int, tuple[float, float, float]]], settings: dict[str, str]) -> str:
    width, height = 900, 392
    # The size panel's floor follows the data rather than being fixed. `py`
    # clamps, so a floor that the band outgrows does not overflow the panel --
    # it flat-bottoms, and the chart quietly reads as a smaller win than the
    # report records. That happened: a hard -18% floor clipped a corpus that
    # had reached -26%. The floor never rises above -18% so that ordinary
    # sweeps keep a stable vertical scale and remain comparable by eye.
    deepest = min(v[1] for v in data["size"].values())
    size_lo = min(-18.0, math.floor((deepest - 2.0) / 5.0) * 5.0)
    size = Panel(66, 94, 340, 200, size_lo, 3.0)
    speed = Panel(534, 94, 340, 200, 55.0, 125.0)

    body: list[str] = []

    # Left: output size. The median sits on zero at every level, so the band is
    # what carries the information -- it is the spread across the corpora.
    body += axes(
        size,
        [0.0] + [-5.0 * step for step in range(1, int(-size_lo // 5) + 1) if -5.0 * step > size_lo],
        "{:+.0f}%",
        "Compressed size vs zstd",
        "lower is better &#8212; below the line means zstandard emitted fewer bytes",
    )
    body.append(
        f'<path d="{size.band({k: v[1] for k, v in data["size"].items()}, {k: v[2] for k, v in data["size"].items()})}" class="bandA"/>'
    )
    body.append(f'<line x1="{size.x}" y1="{size.py(0):.1f}" x2="{size.x + size.w}" y2="{size.py(0):.1f}" class="parity"/>')
    body.append(f'<path d="{size.line({k: v[0] for k, v in data["size"].items()})}" class="lineA"/>')
    body.append(f'<text x="{size.x + 8}" y="{size.py(0) - 10:.1f}" class="note">identical to zstd</text>')
    # Anchored bottom-right: the band's deepest excursions are at the low and
    # middle levels, so the right of the panel is the corner that stays clear.
    body.append(f'<text x="{size.x + size.w - 4}" y="{size.py(size_lo + 1.2):.1f}" class="note end">band: best and worst of the 11 corpora</text>')

    # Right: throughput, as a percentage of upstream on the same bytes.
    body += axes(
        speed,
        [125.0, 100.0, 75.0, 55.0],
        "{:.0f}%",
        "Throughput vs zstd",
        "higher is better &#8212; percentage of upstream on the same bytes",
    )
    body.append(f'<line x1="{speed.x}" y1="{speed.py(100):.1f}" x2="{speed.x + speed.w}" y2="{speed.py(100):.1f}" class="parity"/>')
    # Left of the panel, not right. The encode median now rises above 100% at
    # both ends -- levels 1-2 and again from level 10 -- so the band just above
    # the parity line is never wholly clear; at the left edge the encode line
    # sits far enough above it to leave this label its own gap, and at the right
    # it does not.
    body.append(f'<text x="{speed.x + 6}" y="{speed.py(100) - 9:.1f}" class="note">parity with zstd</text>')
    body.append(f'<path d="{speed.line({k: v[0] for k, v in data["encode"].items()})}" class="lineA"/>')
    body.append(f'<path d="{speed.line({k: v[0] for k, v in data["decode"].items()})}" class="lineB"/>')

    # Inside the panel, top-left: both series sit near 100%, so the space above
    # the parity line is free. Below the panel is not -- the axis label is there.
    legend_y = speed.y + 18
    body.append(f'<line x1="{speed.x + 6:.0f}" y1="{legend_y - 4:.0f}" x2="{speed.x + 30:.0f}" y2="{legend_y - 4:.0f}" class="lineA"/>')
    body.append(f'<text x="{speed.x + 36:.0f}" y="{legend_y:.0f}" class="legend">encode</text>')
    body.append(f'<line x1="{speed.x + 92:.0f}" y1="{legend_y - 4:.0f}" x2="{speed.x + 116:.0f}" y2="{legend_y - 4:.0f}" class="lineB"/>')
    body.append(f'<text x="{speed.x + 122:.0f}" y="{legend_y:.0f}" class="legend">decode</text>')

    # The report omits its revision row when it cannot resolve one, so the
    # stamp is dropped rather than printed as a placeholder: a caption reading
    # "zstandard unknown" says less than one that simply names the crate.
    revision = settings.get("zstandard revision")
    crate = f"zstandard {revision}" if revision else "zstandard"
    upstream = settings.get("Upstream zstd reference", "unknown")
    cases = settings.get("Corpus cases", "?")
    caption = (
        f"{cases} corpora &#215; levels 1-22, one-shot, median across corpora &#183; "
        f"{crate} vs zstd {upstream} &#183; single machine, relative numbers only"
    )

    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" role="img" aria-label="Compressed size and throughput compared with upstream zstd across compression levels 1 to 22">
  <title>zstandard versus upstream zstd</title>
  <desc>Left: compressed size relative to upstream, which is identical at every level with a band showing corpora where zstandard emits less. Right: encode and decode throughput as a percentage of upstream, both near parity.</desc>
  <defs>
    <style>
      /* Generated by scripts/plot_benchmarks.py -- do not edit by hand.
         Keep markup characters out of this comment: XML does not treat style
         content as CDATA, so a literal angle bracket is parsed as a tag. */
      .page    {{ fill: #ffffff; }}
      .grid    {{ stroke: #d8dee4; stroke-width: 1; }}
      .axis    {{ stroke: #afb8c1; stroke-width: 1.2; }}
      .parity  {{ stroke: #8c959f; stroke-width: 1.5; stroke-dasharray: 5 4; }}
      .bandA   {{ fill: #4f46e5; fill-opacity: 0.16; }}
      .lineA   {{ fill: none; stroke: #4f46e5; stroke-width: 2.4; stroke-linejoin: round; stroke-linecap: round; }}
      .lineB   {{ fill: none; stroke: #0891b2; stroke-width: 2.4; stroke-linejoin: round; stroke-linecap: round; }}
      .ptitle  {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 17px; font-weight: 600; fill: #24292f; }}
      .psub    {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 12.5px; fill: #57606a; }}
      .ytick   {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; fill: #57606a; text-anchor: end; }}
      .xtick   {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; fill: #57606a; text-anchor: middle; }}
      .axlabel {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 12.5px; fill: #57606a; text-anchor: middle; }}
      .legend  {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 13px; fill: #24292f; }}
      .note    {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 12px; font-weight: 600; fill: #57606a; }}
      .end     {{ text-anchor: end; }}
      .caption {{ font-family: ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 12px; fill: #6e7781; text-anchor: middle; }}
      @media (prefers-color-scheme: dark) {{
        .page    {{ fill: #0d1117; }}
        .grid    {{ stroke: #262c36; }}
        .axis    {{ stroke: #3d444d; }}
        .parity  {{ stroke: #6e7781; }}
        .bandA   {{ fill: #818cf8; fill-opacity: 0.22; }}
        .lineA   {{ stroke: #818cf8; }}
        .lineB   {{ stroke: #22d3ee; }}
        .ptitle  {{ fill: #e6edf3; }}
        .psub    {{ fill: #9aa4b0; }}
        .ytick   {{ fill: #9aa4b0; }}
        .xtick   {{ fill: #9aa4b0; }}
        .axlabel {{ fill: #9aa4b0; }}
        .legend  {{ fill: #e6edf3; }}
        .note    {{ fill: #9aa4b0; }}
        .caption {{ fill: #8b949e; }}
      }}
    </style>
  </defs>

  <rect x="0" y="0" width="{width}" height="{height}" rx="14" class="page"/>
{chr(10).join('  ' + line for line in body)}

  <text x="{width // 2}" y="{height - 14}" class="caption">{caption}</text>
</svg>
"""


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=pathlib.Path, default=root / "BENCHMARKS.md")
    parser.add_argument("--output", type=pathlib.Path, default=root / "assets" / "benchmarks.svg")
    args = parser.parse_args()

    try:
        cases, settings = parse_report(args.report.read_text())
        data = series(cases)
    except (OSError, ReportError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    args.output.write_text(render(data, settings))
    print(
        f"wrote {args.output.relative_to(root)} from {len(cases)} corpora "
        f"at {settings.get('zstandard revision', 'an unrecorded revision')} vs zstd {settings.get('Upstream zstd reference', '?')}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
