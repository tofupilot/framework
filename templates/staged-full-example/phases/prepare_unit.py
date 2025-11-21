import sys
import time


def prepare_unit(test_api, ui):
    print(f"Preparing unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} prepared", file=sys.stderr)
    return "CONTINUE"
