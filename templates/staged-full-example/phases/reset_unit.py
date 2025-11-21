import sys
import time


def reset_unit(test_api, ui):
    print(f"Resetting unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} reset", file=sys.stderr)
    return "CONTINUE"
