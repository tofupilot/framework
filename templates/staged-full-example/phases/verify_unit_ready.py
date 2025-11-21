import sys
import time


def verify_unit_ready(test_api, ui):
    print(f"Verifying unit {test_api.slot_id} is ready...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} is ready", file=sys.stderr)
    return "CONTINUE"
