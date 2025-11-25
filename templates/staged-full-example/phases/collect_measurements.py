import sys
import time


def collect_measurements(run, ui):
    print(f"Collecting measurements for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Measurements collected for unit {run.slot_id}", file=sys.stderr)
