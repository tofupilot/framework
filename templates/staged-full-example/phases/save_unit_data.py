import sys
import time


def save_unit_data(run, ui):
    print(f"Saving data for unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data saved for unit {run.slot_id}", file=sys.stderr)
