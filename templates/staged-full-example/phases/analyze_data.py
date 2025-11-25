import sys
import time


def analyze_data(run, ui):
    print(f"Analyzing data for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data analysis completed for unit {run.slot_id}", file=sys.stderr)
