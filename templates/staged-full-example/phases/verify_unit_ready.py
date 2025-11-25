import sys
import time


def verify_unit_ready(phase, run, ui):
    print(f"Verifying unit {run.slot_id} is ready...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} is ready", file=sys.stderr)
