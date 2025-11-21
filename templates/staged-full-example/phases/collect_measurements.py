import sys
import time


def collect_measurements(test_api, ui):
    print(f"Collecting measurements for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Measurements collected for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"
