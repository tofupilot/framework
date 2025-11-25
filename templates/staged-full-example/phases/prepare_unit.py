import sys
import time


def prepare_unit(run, ui):
    print(f"Preparing unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} prepared", file=sys.stderr)
