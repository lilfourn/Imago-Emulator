"""Reads the app's timing trace (crates/emu-app/src/trace.rs) from a
bench.sh run and prints what it measured, one `key=value` a line.

Only the stretch from the first tap to just after the last counts, so
startup is left out. Times are in milliseconds:

- latency: from a key's press reaching the app to the window having made
  the images of the first frame that ran with it; the pipeline, the same
  from that frame's start; then its parts: the wait for that frame to
  start, the frame, and from the frame to the window. The display shows
  it at its next refresh after that.
- frames: frames run and drawn a second, redraws a second, frames run but
  never drawn, stalls (a covered window draws nothing until a key wakes it;
  those gaps are left out of the rest), and how evenly frames come: the
  spread of the gaps between
  frames drawn, and how many gaps a second are not the whole number of
  the display's refreshes that each frame should show for.
- work: each frame's run and the emulation thread's whole turn, how late
  the thread woke, the window's time making images, and CPU used (percent
  of one core).
"""

import argparse
import statistics

# Milliseconds without a frame drawn that count as a stall.
STALL = 100


def pct(values, p):
    if not values:
        return float("nan")
    values = sorted(values)
    return values[min(len(values) - 1, int(p / 100 * len(values)))]


def rate(events):
    """Events a second, from the first of `events` to the last."""
    if len(events) < 2:
        return float("nan")
    return (len(events) - 1) / (events[-1][0] - events[0][0]) * 1000


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("trace")
    parser.add_argument("--wall", type=float, default=0)
    parser.add_argument("--taps", type=int, default=0)
    parser.add_argument("--app-cpu", type=float)
    parser.add_argument("--player-cpu", type=float)
    parser.add_argument("--refresh-hz", type=float, default=120)
    args = parser.parse_args()

    events = []
    with open(args.trace) as trace:
        for line in trace:
            parts = line.split()
            if len(parts) >= 2:
                events.append((int(parts[0]) / 1000, parts[1], [int(v) for v in parts[2:]]))
    sends = [e for e in events if e[1] == "send"]
    if not sends:
        print("error=no taps reached the app")
        return
    # Until just after the last tap, or the trace's end if that is sooner.
    start, stop = sends[0][0], min(sends[-1][0] + 500, events[-1][0])
    events = [e for e in events if start <= e[0] <= stop]
    seconds = (stop - start) / 1000

    def of(kind):
        return [e for e in events if e[1] == kind]

    runs, takes, drawn = of("run"), of("take"), of("drawn")
    drawn_at = {e[2][0]: e[0] for e in drawn}
    gaps = [b[0] - a[0] for a, b in zip(runs, runs[1:])]
    period = statistics.median(gaps) if gaps else float("nan")

    # Latency: each press, to the first frame drawn that ran with it.
    latencies, waits, frames, to_window = [], [], [], []
    held = 0
    for t0, _, (buttons,) in sends:
        pressed = buttons & ~held
        held = buttons
        if not pressed:
            continue
        run = next((r for r in runs if r[0] > t0 and r[2][1] & pressed == pressed), None)
        if run is None:
            continue
        frame, _, took = run[2]
        began = run[0] - took / 1000
        if began < t0:
            # It ran across the press, so the press missed it: rare, as a
            # frame's buttons are read as it starts.
            continue
        shown = next((t for seq, t in sorted(drawn_at.items()) if seq >= frame and t >= run[0]), None)
        if shown is None:
            continue
        latencies.append(shown - t0)
        waits.append(began - t0)
        frames.append(took / 1000)
        to_window.append(shown - run[0])

    # Without the wait for the frame to start, which is however long is
    # left of the frame the key landed in: what the app's work adds.
    after = [latency - wait for latency, wait in zip(latencies, waits)]

    # A window covered by others draws nothing until a key wakes it: those
    # stalls are counted apart, not as uneven frames.
    pairs = [(a, b) for a, b in zip(takes, takes[1:]) if b[0] - a[0] < STALL]
    stalls = len(takes) - 1 - len(pairs)
    take_gaps = [b[0] - a[0] for a, b in pairs]
    skipped = sum(max(0, b[2][0] - a[2][0] - 1) for a, b in pairs)
    # Each frame should show for the same whole number of refreshes.
    refresh = 1000 / args.refresh_hz
    expected = refresh * max(1, round(period / refresh))
    uneven = sum(1 for gap in take_gaps if abs(gap - expected) > refresh / 2)
    run_ms = [r[2][2] / 1000 for r in runs]
    busy = [e[2][0] / 1000 for e in of("busy")]
    late = [e[2][0] / 1000 for e in of("late")]
    made = [e[2][1] / 1000 for e in drawn]

    out = {
        "seconds": seconds,
        "taps": args.taps,
        "presses_measured": len(latencies),
        "latency_ms_median": statistics.median(latencies) if latencies else float("nan"),
        "latency_ms_mean": statistics.fmean(latencies) if latencies else float("nan"),
        "latency_ms_p95": pct(latencies, 95),
        "pipeline_ms_median": statistics.median(after) if after else float("nan"),
        "pipeline_ms_p95": pct(after, 95),
        "wait_for_frame_ms_mean": statistics.fmean(waits) if waits else float("nan"),
        "frame_ms_mean": statistics.fmean(frames) if frames else float("nan"),
        "frame_to_window_ms_mean": statistics.fmean(to_window) if to_window else float("nan"),
        "frame_period_ms": period,
        "fps_run": rate(runs),
        "fps_drawn": rate(takes),
        "redraws_per_s": len(of("render")) / seconds,
        "frames_never_drawn": skipped,
        "stalls": stalls,
        "draw_gap_ms_stdev": statistics.pstdev(take_gaps) if take_gaps else float("nan"),
        "run_gap_ms_stdev": statistics.pstdev(gaps) if gaps else float("nan"),
        "uneven_gaps_per_s": uneven / seconds,
        "run_ms_median": statistics.median(run_ms) if run_ms else float("nan"),
        "run_ms_p95": pct(run_ms, 95),
        "busy_ms_median": statistics.median(busy) if busy else float("nan"),
        "busy_ms_p95": pct(busy, 95),
        "late_ms_p50": pct(late, 50),
        "late_ms_p99": pct(late, 99),
        "images_ms_median": statistics.median(made) if made else float("nan"),
        "images_ms_p95": pct(made, 95),
    }
    if args.wall and args.app_cpu is not None:
        out["app_cpu_percent"] = args.app_cpu / args.wall * 100
    if args.wall and args.player_cpu is not None:
        out["player_cpu_percent"] = args.player_cpu / args.wall * 100
    for key, value in out.items():
        print(f"{key}={value:.2f}" if isinstance(value, float) else f"{key}={value}")


main()
