import sys
import time


def reset_unit(run, ui):
    print(f"Resetting unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} reset", file=sys.stderr)
